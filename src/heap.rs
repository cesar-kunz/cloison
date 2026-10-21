//! The heap the collector manages (`docs/gc.md`).
//!
//! All heaps of the process share one reservation of address space; each
//! takes a slice of it, carved into 32 KiB blocks and committed as used. A
//! cell up to [`LARGE`] bytes is allocated by bumping a pointer through a
//! *hole* (free granules) of a block, and never crosses a block's end; a
//! larger one gets a run of blocks of its own. Cells are 16-byte aligned
//! and sized in 16-byte granules, and never move.
//!
//! Beside the reservation, two side tables, shared too, so that the write
//! barrier needs no heap at hand ([`card_mark`]):
//!
//! - **Metadata**, one byte per granule ([`meta`]). Allocation writes
//!   [`YOUNG`] at a cell's first granule; a collection that finds the cell
//!   alive replaces it with the mark color ([`COLOR`]) and writes [`END`]
//!   at the cell's last granule. So a granule's byte says whether a cell
//!   starts there and how old it is, without reading the cell: what a
//!   conservative root is checked against, and what holes are found from.
//! - **Cards**, one byte per [`CARD`] bytes, set by a store into an old
//!   cell of a cell not marked with the current color ([`barrier`]): what
//!   a minor collection scans old cells for young referents in.
//!
//! Every cell says how big it is from its first words ([`cell_size`]): an
//! object starts with its shape's address (8-aligned), anything else with
//! a header word whose low bit is set ([`VALUES`], [`BYTES`], [`UPVALUE`]).
//! Pointers to arrays of values (an object's `props` and `elems`) and to
//! upvalues point past that header word, at the payload.
//!
//! A hole's memory is not zeroed when allocation takes it: each cell's
//! maker writes all of it a collection reads (an object's header and
//! slots, an array's values; a string's units are never read past its
//! length), and zeroes what must start zero (a byte buffer's).
//!
//! The shared reservation sits where its side tables can be at a 16th and
//! a 128th of its addresses past one constant ([`SIDE`]): a granule's
//! metadata byte is at `SIDE + (addr >> 4)`, a card at `SIDE + (addr >>
//! 7)`, and the barrier needs no operands.
//!
//! Compiled code allocates too, bumping `ptr` while it stays within `end`
//! and writing the metadata byte of the new cell: they come first, in that
//! order (see [`Heap::bump`]).

use crate::object::{Class, INLINE_BASE, Object, Shape, object_header};
use crate::value::Value;

// What compiled code knows of the heap is defined with the other layouts
// it shares: the granule, card and large-cell sizes, the metadata bytes,
// the side tables' place, the address space of a heap.
pub use crate::layout::{CARD, COLOR, GRANULE, LARGE, SIDE, SLICE, VALUES, YOUNG, card, card_mark, color_byte, meta};
/// Block size: allocation's unit of memory.
pub const BLOCK: usize = 32 << 10;
const GRANULES_PER_BLOCK: usize = BLOCK / GRANULE;
/// Address space all heaps share: room for 512 heaps of a [`SLICE`] each.
/// Only reserved (no page is touched until committed); from any of
/// [`BASES`] it ends below 128 TiB, and its side tables stay between
/// [`SIDE`] and the first base.
const SPACE: usize = 16 << 40;
/// Memory is committed this much at a time (its metadata and cards are
/// then whole pages).
const CHUNK: usize = 8 << 20;

/// The system's page size: what `mmap`, `mprotect` and `munmap` round to
/// (16 KiB on Apple silicon, 4 KiB elsewhere).
pub(crate) const PAGE: usize = if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
    16 << 10
} else {
    4 << 10
};

/// Metadata bit of an old cell's last granule.
pub const END: u8 = 0x04;

/// Header word of a byte buffer: `BYTES | length << 32`.
pub const BYTES: u64 = 3;
/// Header word of an upvalue (`vm::Upvalue` follows).
pub const UPVALUE: u64 = 5;
/// Header word of a weak collection's entries (see `collections`): as
/// [`VALUES`]'s, but the collector traces only its header slots, leaving
/// the keys and values to the ephemeron pass (`Vm::ephemerons`).
pub const WEAK_VALUES: u64 = 7;
const KIND: u64 = 7;

/// A block is allocated in again after a minor collection only if it took
/// at least this many bytes of holes in the cycle before, and after a full
/// one only if that much of it is free.
const RECYCLE: usize = 4 << 10;

/// Holes smaller than this are left unused until the next collection.
pub(crate) fn min_hole() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("CLOISON_GC_MIN_HOLE").ok().and_then(|s| s.parse().ok()).unwrap_or(256))
}

/// What a block holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    /// Nothing: its memory may hold dead cells.
    Free,
    /// Cells of up to [`LARGE`] bytes.
    Small,
    /// The first of a large cell's `n` blocks.
    LargeHead(u32),
    LargeTail,
    /// The slice's first block: no cells; the metadata byte of its first
    /// granule is the heap's current color (see [`barrier`]).
    Header,
}

/// The reservation all heaps share, and its side tables.
struct Space {
    base: usize,
    /// Slices no heap has, by index.
    free: std::sync::Mutex<Vec<usize>>,
}

static SPACE_: std::sync::OnceLock<Space> = std::sync::OnceLock::new();

/// `len` bytes of address space at `at`, inaccessible until committed,
/// if they are free.
fn reserve_at(at: usize, len: usize) -> bool {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
    let p = unsafe { libc::mmap(at as *mut libc::c_void, len, libc::PROT_NONE, flags, -1, 0) };
    if p == libc::MAP_FAILED {
        return false;
    }
    if p as usize != at {
        unsafe { libc::munmap(p, len) };
        return false;
    }
    true
}

/// Candidate addresses for the reservation (16, 32, 48, 64 TiB): above
/// [`SIDE`] plus their side tables, all where Linux and macOS leave room.
const BASES: [usize; 4] = [0x1000 << 32, 0x2000 << 32, 0x3000 << 32, 0x4000 << 32];

fn space() -> &'static Space {
    SPACE_.get_or_init(|| {
        for base in BASES {
            if !reserve_at(base, SPACE) {
                continue;
            }
            if !reserve_at(SIDE + base / GRANULE, SPACE / GRANULE) {
                unsafe { libc::munmap(base as *mut libc::c_void, SPACE) };
                continue;
            }
            if !reserve_at(SIDE + base / CARD, SPACE / CARD) {
                unsafe {
                    libc::munmap(base as *mut libc::c_void, SPACE);
                    libc::munmap((SIDE + base / GRANULE) as *mut libc::c_void, SPACE / GRANULE);
                }
                continue;
            }
            return Space {
                base,
                free: std::sync::Mutex::new((0..SPACE / SLICE).rev().collect()),
            };
        }
        panic!("no room for the heap's reservation at any of {BASES:x?}");
    })
}

/// Make `len` bytes at `a` (chunk-aligned) usable, with their metadata
/// and cards. Fresh memory is zero.
fn commit(a: usize, len: usize) {
    let rw = libc::PROT_READ | libc::PROT_WRITE;
    for (p, n) in [(a, len), (meta(a) as usize, len / GRANULE), (card(a) as usize, len / CARD)] {
        let r = unsafe { libc::mprotect(p as *mut libc::c_void, n, rw) };
        assert!(r == 0, "heap commit failed");
    }
}

/// Give `len` bytes at `a` (chunk-aligned) back to the system with their
/// metadata and cards, inaccessible and zero when committed again.
fn decommit(a: usize, len: usize) {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE | libc::MAP_FIXED;
    for (p, n) in [(a, len), (meta(a) as usize, len / GRANULE), (card(a) as usize, len / CARD)] {
        let r = unsafe { libc::mmap(p as *mut libc::c_void, n, libc::PROT_NONE, flags, -1, 0) };
        assert!(r != libc::MAP_FAILED, "heap decommit failed");
    }
}

/// Let the system take back the memory of `len` bytes at `a` (blocks that
/// hold nothing, which are zeroed when used again).
fn release(a: usize, len: usize) {
    #[cfg(target_os = "macos")]
    let advice = libc::MADV_FREE_REUSABLE;
    #[cfg(not(target_os = "macos"))]
    let advice = libc::MADV_DONTNEED;
    unsafe { libc::madvise(a as *mut libc::c_void, len, advice) };
}

/// The write barrier: `v` was stored at `a`, in the cell at `host`. A
/// reference from an old cell to one that isn't marked with the heap's
/// current color is recorded, by marking the card of `a`: outside a
/// marking cycle, a young cell (the next minor collection must find it);
/// during one, also an old cell not marked yet (the marker may have traced
/// `host` already). The current color is the metadata byte of the heap's
/// slice's first granule ([`color_byte`]).
#[inline(always)]
pub fn barrier<T>(host: *const u8, a: *const T, v: Value) {
    unsafe {
        if v.is_cell()
            && *meta(v.to_bits() as usize) & (YOUNG | COLOR) != *color_byte(host as usize)
            && *meta(host as usize) != YOUNG
        {
            card_mark(a);
        }
    }
}

/// The write barrier for a store at `a`, in the cell at `host`, of a
/// pointer to a young array (an object's new `props` or `elems`).
#[inline(always)]
pub fn barrier_young<T>(host: *const u8, a: *const T) {
    if unsafe { *meta(host as usize) } != YOUNG {
        card_mark(a);
    }
}

crate::layout::same_layout!(Heap, crate::layout::HeapPtrs: ptr, end);

#[repr(C)]
pub struct Heap {
    // Read by compiled code (`Heap::bump`, `layout::HeapPtrs`): keep first,
    // in this order.
    ptr: *mut u8,
    end: *mut u8,
    /// Where the current hole started (allocations since are a region).
    hole: *mut u8,
    /// The slice's start, its index, and how much of it may be used.
    base: usize,
    slice: usize,
    limit: usize,
    /// Bytes from `base` committed so far.
    committed: usize,
    /// Blocks handed out of the slice so far.
    top: usize,
    blocks: Vec<Block>,
    /// Blocks with free granules to allocate in, and the one being
    /// allocated in: where its next hole may start, and its end.
    recycle: Vec<u32>,
    cursor: usize,
    cursor_end: usize,
    /// Blocks that hold nothing: given back to the system (their memory
    /// zero, their metadata not), and kept (see `add_swept`).
    free: Vec<u32>,
    empty: Vec<u32>,
    /// The ranges cells were allocated in since the last collection, in
    /// order, the current hole's not yet (its end is `ptr`).
    pub(crate) regions: Vec<(usize, usize)>,
    /// The room a minor collection left in each of `regions` (see
    /// `free_young`).
    pub(crate) room: Vec<usize>,
    /// Bytes handed out since the last collection, and blocks in use.
    since: usize,
    used: usize,
    /// Bytes handed out after which the next collection is requested.
    pub(crate) budget: usize,
    /// A collection is due at the next safepoint (`Vm::safepoint`); the
    /// marker thread sets it too, for the final pause.
    pub(crate) requested: std::sync::atomic::AtomicBool,
    /// The color the latest collection marked with (`COLOR`).
    pub(crate) color: u8,
    /// Since the last collection: holes taken, holes too small for the
    /// cell asked for, granules looked at to find them (`CLOISON_GC_STATS`).
    pub(crate) holes: [usize; 3],
    /// Since the last full collection's sweep began: blocks it left with
    /// cells, the bytes free in them, those of them allocated in again,
    /// and blocks it emptied (`CLOISON_GC_STATS`).
    pub(crate) swept: [usize; 4],
    pub(crate) gc: crate::gc::Gc,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        // The marker thread first: it reads the heap.
        self.gc.marker = None;
        decommit(self.base, self.committed);
        space().free.lock().unwrap().push(self.slice);
    }
}

impl Heap {
    pub fn new() -> Heap {
        let limit = std::env::var("CLOISON_HEAP_LIMIT")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map_or(SLICE, |mb| (mb << 20).clamp(CHUNK, SLICE));
        let s = space();
        // (The lock is released before the check: a panic holding it would
        // poison it for every heap made or dropped after.)
        let slice = s.free.lock().unwrap().pop();
        let slice = slice.unwrap_or_else(|| panic!("more than {} heaps alive at once", SPACE / SLICE));
        let gc = crate::gc::Gc::new();
        let mut heap = Heap {
            budget: gc.budget(),
            holes: [0; 3],
            swept: [0; 4],
            requested: std::sync::atomic::AtomicBool::new(false),
            color: 1,
            gc,
            ptr: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
            hole: std::ptr::null_mut(),
            base: s.base + slice * SLICE,
            slice,
            limit,
            committed: 0,
            top: 0,
            blocks: Vec::new(),
            recycle: Vec::new(),
            cursor: 0,
            cursor_end: 0,
            free: Vec::new(),
            empty: Vec::new(),
            regions: Vec::new(),
            room: Vec::new(),
            since: 0,
            used: 0,
        };
        // The header block, and the current color in it.
        let b = heap.fresh_blocks(1);
        heap.blocks[b] = Block::Header;
        heap.set_color(1);
        heap
    }

    /// Make `c` the heap's current color (see [`barrier`]).
    pub(crate) fn set_color(&mut self, c: u8) {
        self.color = c;
        unsafe { *color_byte(self.base) = c };
    }

    /// Where compiled code finds the free space: `ptr`, then `end`.
    pub(crate) fn bump(&mut self) -> *mut crate::layout::HeapPtrs {
        (&raw mut self.ptr).cast()
    }

    /// Whether `a` is an address in the heap.
    #[inline(always)]
    pub fn contains(&self, a: usize) -> bool {
        // (Not the header block: its color byte would read as a cell's.)
        a.wrapping_sub(self.base + BLOCK) < self.top.saturating_sub(1) * BLOCK
    }

    /// The metadata byte of the granule at `a` (in the heap).
    #[inline(always)]
    pub fn meta(&self, a: usize) -> *mut u8 {
        meta(a)
    }

    /// A new cell of `bytes` (a multiple of [`GRANULE`]), its metadata
    /// [`YOUNG`]: what it holds is left from before, for the caller to
    /// write all a collection may read.
    #[inline]
    pub fn alloc(&mut self, bytes: usize) -> *mut u8 {
        debug_assert!(bytes.is_multiple_of(GRANULE) && bytes > 0);
        let p = self.ptr;
        // (Past `LARGE`, a cell of its own blocks: in a small block, a card
        // that far into it would find no cell start, see
        // `cell_start_before`.)
        if bytes > LARGE || (self.end as usize - p as usize) < bytes {
            return self.alloc_slow(bytes);
        }
        self.ptr = unsafe { p.add(bytes) };
        unsafe { *meta(p as usize) = YOUNG };
        p
    }

    #[cold]
    #[inline(never)]
    fn alloc_slow(&mut self, bytes: usize) -> *mut u8 {
        if bytes > LARGE {
            return self.alloc_large(bytes);
        }
        self.end_region();
        let (s, e) = loop {
            // Smaller holes would send compiled code's inline allocation
            // to the runtime every few cells.
            if let Some((s, e)) = self.next_hole(bytes.max(min_hole())) {
                self.holes[0] += 1;
                break (s, e);
            }
            if let Some(b) = self.recycle.pop() {
                self.cursor = self.block_addr(b as usize) as usize;
                self.cursor_end = self.cursor + BLOCK;
                continue;
            }
            // Blocks the marker thread swept since: allocate in those, or
            // sweep some here before taking a new one.
            if self.gc.sweeping {
                self.absorb_sweep();
                if !self.recycle.is_empty() || (self.free.is_empty() && self.empty.is_empty() && self.help_sweep()) {
                    continue;
                }
            }
            let b = if let Some(b) = self.empty.pop() {
                self.clean(b as usize, 1);
                b as usize
            } else if let Some(b) = self.free.pop() {
                unsafe { std::ptr::write_bytes(meta(self.block_addr(b as usize) as usize), 0, BLOCK / GRANULE) };
                b as usize
            } else {
                self.fresh_blocks(1)
            };
            self.blocks[b] = Block::Small;
            self.used += BLOCK;
            let a = self.block_addr(b) as usize;
            break (a, a + BLOCK);
        };
        self.hole = s as *mut u8;
        self.ptr = s as *mut u8;
        self.end = e as *mut u8;
        self.since += e - s;
        if self.since >= self.budget {
            self.requested.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.alloc(bytes)
    }

    /// The next hole of at least `min` bytes in the block being allocated
    /// in: a run of granules no cell covers.
    ///
    /// From the metadata alone: a block allocation comes back to holds old
    /// cells only (a collection made every survivor old), each with its
    /// color at its first granule and `END` at its last, and 0 elsewhere.
    /// Eight granules' bytes are looked at at a time.
    fn next_hole(&mut self, min: usize) -> Option<(usize, usize)> {
        const ENDS: u64 = 0x0404_0404_0404_0404;
        let end = self.cursor_end;
        let mut p = self.cursor;
        let from = p;
        unsafe {
            while p < end {
                let m = *meta(p);
                if m != 0 {
                    // A cell: on to the granule after its `END`.
                    debug_assert!(m & COLOR != 0, "a young cell where allocation comes back");
                    // (Eight bytes at a time within the block: the
                    // metadata past the heap's end may not be mapped.)
                    while p + 8 * GRANULE <= end {
                        let w = (meta(p) as *const u64).read_unaligned() & ENDS;
                        if w != 0 {
                            p += w.trailing_zeros() as usize / 8 * GRANULE;
                            break;
                        }
                        p += 8 * GRANULE;
                    }
                    while *meta(p) & END == 0 {
                        p += GRANULE;
                    }
                    p += GRANULE;
                    continue;
                }
                let s = p;
                // Free granules: the bytes up to the next nonzero one.
                loop {
                    if p + 8 * GRANULE <= end {
                        let w = (meta(p) as *const u64).read_unaligned();
                        if w == 0 {
                            p += 8 * GRANULE;
                            continue;
                        }
                        p += w.trailing_zeros() as usize / 8 * GRANULE;
                        break;
                    }
                    if p == end || *meta(p) != 0 {
                        break;
                    }
                    p += GRANULE;
                }
                if p - s < min {
                    self.holes[1] += 1;
                    continue;
                }
                self.cursor = p;
                self.holes[2] += (p - from) / GRANULE;
                return Some((s, p));
            }
        }
        self.holes[2] += (end - from) / GRANULE;
        self.cursor = end;
        None
    }

    /// A collection finished: count afresh, give the blocks allocated in
    /// since the last one to allocation again, and request the next one
    /// after `budget` bytes.
    pub(crate) fn collected(&mut self, budget: usize, full: bool) {
        self.end_region();
        // The current hole's unused end is room too.
        let tail = (self.end as usize).saturating_sub(self.ptr as usize);
        if tail > 0 {
            self.regions.push((self.ptr as usize, self.end as usize));
        }
        if !full {
            // Once each: a block a second time would be allocated in again
            // over cells it holds since (holes of one block need not be
            // consecutive regions: a large cell's comes between). And only
            // a block with at least `RECYCLE` bytes of room in them, in
            // holes allocation takes: one with small holes between old
            // cells costs a scan of its metadata for little room (Splay's
            // blocks of promoted nodes, scanned at every cycle, were a
            // tenth of its time). The room `free_young` left, not what the
            // regions held: most of Splay's young cells survive, and its
            // allocation scanned twice the granules counting those. (The
            // hole's unused end, after them, is all room.)
            let mut bs: Vec<(u32, usize)> = (self.regions.iter().enumerate())
                .map(|(k, &(s, e))| (((s - self.base) / BLOCK) as u32, self.room.get(k).map_or(e - s, |&r| r)))
                .filter(|&(b, _)| self.blocks[b as usize] == Block::Small)
                .collect();
            bs.sort_unstable();
            let mut k = 0;
            while k < bs.len() {
                let b = bs[k].0;
                let mut room = 0;
                while k < bs.len() && bs[k].0 == b {
                    room += bs[k].1;
                    k += 1;
                }
                if room >= RECYCLE {
                    self.recycle.push(b);
                }
            }
            self.recycle.reverse();
        }
        self.regions.clear();
        self.room.clear();
        // The block being allocated in is among them: start afresh.
        self.ptr = std::ptr::null_mut();
        self.end = std::ptr::null_mut();
        self.hole = std::ptr::null_mut();
        self.cursor = 0;
        self.cursor_end = 0;
        self.since = 0;
        self.budget = budget;
        self.requested.store(false, std::sync::atomic::Ordering::Relaxed);
    }

    /// Bytes handed out since the last collection.
    pub(crate) fn since(&self) -> usize {
        self.since
    }

    /// Where cells may be: from the block after the header to the end of
    /// the last handed out.
    pub(crate) fn cells_range(&self) -> (usize, usize) {
        (self.base + BLOCK, self.base + self.top * BLOCK)
    }

    /// The heap's address range: from its first block to the end of the
    /// last handed out.
    pub(crate) fn space_range(&self) -> (usize, usize) {
        (self.base, self.base + self.top * BLOCK)
    }

    /// Whether `p` is a large cell's first granule.
    pub(crate) fn is_large_start(&self, p: usize) -> bool {
        p.is_multiple_of(BLOCK) && matches!(self.blocks[(p - self.base) / BLOCK], Block::LargeHead(_))
    }

    /// After a full collection: allocate in the blocks at `partly` again
    /// (with their free bytes), those with room (see [`RECYCLE`]), and free
    /// those at `empty`, giving their memory back to the system.
    pub(crate) fn reset_blocks(&mut self, partly: &[(usize, usize)], empty: &[usize]) {
        self.recycle.clear();
        self.add_swept(partly, empty);
    }

    /// Blocks a sweep found: allocate in those at `partly` again (with
    /// their free bytes) if they have room (see [`RECYCLE`]), and free
    /// those at `empty`. Up to the room's bytes of free blocks keep their
    /// memory, the heap growing by about that much before the next cycle
    /// (`Gc::room`): the others' goes back to the system. (Splay, whose
    /// sweeps empty a hundred MiB a cycle that it allocates in again, took
    /// 444,000 page faults a run giving all back.)
    pub(crate) fn add_swept(&mut self, partly: &[(usize, usize)], empty: &[usize]) {
        self.swept[0] += partly.len();
        self.swept[1] += partly.iter().map(|&(_, free)| free).sum::<usize>();
        self.swept[2] += partly.iter().filter(|&&(_, free)| free >= RECYCLE).count();
        self.swept[3] += empty.len();
        self.recycle.extend(
            (partly.iter().rev())
                .filter(|&&(_, free)| free >= RECYCLE)
                .map(|&(a, _)| ((a - self.base) / BLOCK) as u32),
        );
        let keep = self.kept_blocks();
        let mut empty = empty.to_vec();
        empty.sort_unstable();
        let mut run: Option<(usize, usize)> = None;
        for &a in &empty {
            let b = (a - self.base) / BLOCK;
            self.blocks[b] = Block::Free;
            self.used -= BLOCK;
            if self.empty.len() < keep {
                self.empty.push(b as u32);
                continue;
            }
            self.free.push(b as u32);
            // Contiguous blocks go back in one call.
            run = match run {
                Some((s, e)) if e == a => Some((s, a + BLOCK)),
                Some((s, e)) => {
                    release(s, e - s);
                    Some((a, a + BLOCK))
                }
                None => Some((a, a + BLOCK)),
            };
        }
        if let Some((s, e)) = run {
            release(s, e - s);
        }
    }

    /// Free blocks that keep their memory (see `add_swept`).
    pub(crate) fn kept_empty(&self) -> usize {
        self.empty.len()
    }

    /// How many free blocks keep their memory: the room the heap may grow
    /// by before the next cycle (see `add_swept`).
    fn kept_blocks(&self) -> usize {
        self.gc.room.max(self.gc.headroom()) / BLOCK
    }

    /// Free the large cell at `p` (its blocks become free, kept like an
    /// empty block's while there's room: see `add_swept`).
    pub(crate) fn free_large(&mut self, p: usize) {
        let b = (p - self.base) / BLOCK;
        let Block::LargeHead(n) = self.blocks[b] else { panic!("no large cell at {p:#x}") };
        let n = n as usize;
        unsafe { *meta(p) = 0 };
        let kept = self.empty.len() + n <= self.kept_blocks();
        for k in b..b + n {
            self.blocks[k] = Block::Free;
            if kept { self.empty.push(k as u32) } else { self.free.push(k as u32) }
        }
        self.used -= n * BLOCK;
        if !kept {
            release(p, n * BLOCK);
        }
    }

    /// The address range of each block in use: a small block's, or a large
    /// cell's first granule.
    pub(crate) fn block_ranges(&self) -> Vec<(usize, usize)> {
        let mut v = Vec::with_capacity(self.blocks.len());
        for (b, &k) in self.blocks.iter().enumerate() {
            let a = self.base + b * BLOCK;
            match k {
                Block::Small => v.push((a, a + BLOCK)),
                Block::LargeHead(_) => v.push((a, a + GRANULE)),
                Block::LargeTail | Block::Free | Block::Header => {}
            }
        }
        v
    }

    /// The first granule of the cell that address `a` (in the heap) may be
    /// in: the nearest one before it where a cell starts, unless a cell
    /// ends in between.
    pub(crate) fn cell_start_before(&self, a: usize) -> Option<usize> {
        let mut b = (a - self.base) / BLOCK;
        match *self.blocks.get(b)? {
            Block::Free | Block::Header => None,
            Block::Small => {
                let block = self.base + b * BLOCK;
                let g = a & !(GRANULE - 1);
                let mut p = g;
                loop {
                    let m = unsafe { *meta(p) };
                    if m & (YOUNG | COLOR) != 0 {
                        return Some(p);
                    }
                    if m & END != 0 && p < g || p == block || g - p >= LARGE {
                        return None;
                    }
                    p -= GRANULE;
                }
            }
            Block::LargeHead(_) | Block::LargeTail => {
                while self.blocks[b] == Block::LargeTail {
                    b -= 1;
                }
                let p = self.base + b * BLOCK;
                (unsafe { *meta(p) } & (YOUNG | COLOR) != 0).then_some(p)
            }
        }
    }

    /// Record the current hole's allocations as a region.
    pub(crate) fn end_region(&mut self) {
        if self.ptr > self.hole {
            self.regions.push((self.hole as usize, self.ptr as usize));
        }
        self.hole = self.ptr;
    }

    /// A large cell: blocks of its own.
    fn alloc_large(&mut self, bytes: usize) -> *mut u8 {
        let n = bytes.div_ceil(BLOCK);
        let b = self.free_run(n).unwrap_or_else(|| self.fresh_blocks(n));
        self.blocks[b] = Block::LargeHead(n as u32);
        for k in 1..n {
            self.blocks[b + k] = Block::LargeTail;
        }
        let p = self.block_addr(b);
        self.regions.push((p as usize, p as usize + bytes));
        self.since += n * BLOCK;
        self.used += n * BLOCK;
        if self.since >= self.budget {
            self.requested.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        unsafe { *meta(p as usize) = YOUNG };
        p
    }

    /// `n` contiguous free blocks, zeroed and taken off the free list: the
    /// first's index.
    fn free_run(&mut self, n: usize) -> Option<usize> {
        if self.free.len() + self.empty.len() < n {
            return None;
        }
        let mut run = 0;
        let b = (0..self.top).find(|&b| {
            run = if self.blocks[b] == Block::Free { run + 1 } else { 0 };
            run == n
        })? + 1
            - n;
        let run = b as u32..(b + n) as u32;
        self.free.retain(|k| !run.contains(k));
        // Only the kept blocks hold old bytes: released ones are zero.
        let mut kept = Vec::new();
        self.empty.retain(|&k| !run.contains(&k) || {
            kept.push(k);
            false
        });
        unsafe { std::ptr::write_bytes(meta(self.block_addr(b) as usize), 0, n * BLOCK / GRANULE) };
        for k in kept {
            unsafe { std::ptr::write_bytes(self.block_addr(k as usize), 0, BLOCK) };
        }
        Some(b)
    }

    /// Zero `n` blocks from `b` that held cells, with their metadata.
    fn clean(&mut self, b: usize, n: usize) {
        let a = self.block_addr(b);
        unsafe { std::ptr::write_bytes(meta(a as usize), 0, n * BLOCK / GRANULE) };
    }

    /// `n` contiguous blocks never used before: the first's index.
    fn fresh_blocks(&mut self, n: usize) -> usize {
        let b = self.top;
        let end = (b + n) * BLOCK;
        assert!(
            end <= self.limit,
            "out of memory: the heap is limited to {} MiB (CLOISON_HEAP_LIMIT)",
            self.limit >> 20
        );
        if end > self.committed {
            let more = (end - self.committed).next_multiple_of(CHUNK);
            commit(self.base + self.committed, more);
            self.committed += more;
        }
        self.top += n;
        self.blocks.resize(self.top, Block::Free);
        b
    }

    fn block_addr(&self, b: usize) -> *mut u8 {
        (self.base + b * BLOCK) as *mut u8
    }

    /// A new object of `shape`, with its in-object slots `undefined`.
    pub fn alloc_object(&mut self, shape: &Shape) -> *mut Object {
        self.alloc_object_extra(shape, 0)
    }

    /// A new object of `shape` with `extra` words after its in-object
    /// slots (a closure's upvalues, a string's units), the slots
    /// `undefined`, the extra words zero.
    pub fn alloc_object_extra(&mut self, shape: &Shape, extra: usize) -> *mut Object {
        let cap = shape.capacity.get() as usize;
        let o = self.alloc(object_bytes(cap + extra)).cast::<Object>();
        unsafe {
            o.cast::<[u64; INLINE_BASE / 8]>().write(object_header(shape));
            for i in 0..cap {
                Object::inline(o, i).write(Value::UNDEFINED);
            }
            std::ptr::write_bytes(Object::inline(o, cap), 0, extra);
        }
        o
    }

    /// Room for `n` values, all `undefined`: the payload of an array cell.
    pub fn alloc_values(&mut self, n: usize) -> *mut Value {
        self.alloc_values_from(n, 0)
    }

    /// [`Heap::alloc_values`], the values before `from` left for the caller
    /// to write before anything can collect (a growing array's copied
    /// elements: not written twice).
    pub fn alloc_values_from(&mut self, n: usize, from: usize) -> *mut Value {
        let cap = u32::try_from(n).expect("array too large") as u64;
        let p = self.alloc((8 + 8 * n).next_multiple_of(GRANULE)).cast::<u64>();
        unsafe {
            *p = VALUES | cap << 32;
            let v = p.add(1).cast::<Value>();
            for i in from.min(n)..n {
                v.add(i).write(Value::UNDEFINED);
            }
            v
        }
    }

    /// `n` zero bytes (8-aligned): the payload of a byte buffer cell.
    pub fn alloc_bytes(&mut self, n: usize) -> *mut u8 {
        let len = u32::try_from(n).expect("buffer too large") as u64;
        let bytes = (8 + n).next_multiple_of(GRANULE);
        let p = self.alloc(bytes).cast::<u64>();
        unsafe {
            *p = BYTES | len << 32;
            std::ptr::write_bytes(p.add(1).cast::<u8>(), 0, bytes - 8);
            p.add(1).cast()
        }
    }

    /// Room for an upvalue (`vm::Upvalue`, two words), zero: the payload
    /// of an upvalue cell.
    pub fn alloc_upvalue(&mut self) -> *mut u8 {
        let p = self.alloc(32).cast::<u64>();
        unsafe {
            *p = UPVALUE;
            std::ptr::write_bytes(p.add(1), 0, 3);
            p.add(1).cast()
        }
    }

    /// Bytes of blocks in use.
    pub fn allocated(&self) -> usize {
        self.used
    }

    /// Blocks: handed out, to allocate in again, free, large.
    pub fn block_counts(&self) -> (usize, usize, usize, usize) {
        let large = self.blocks.iter().filter(|b| !matches!(b, Block::Small | Block::Free)).count();
        (self.top, self.recycle.len(), self.free.len() + self.empty.len(), large)
    }

    /// Walk every cell, checking that each says how big it is and that
    /// the metadata agrees: a first granule `YOUNG` or colored, an old
    /// cell's last granule `END`, nothing else (for tests). With
    /// `CLOISON_HEAP_HIST` set, print the cells by kind and size.
    pub fn verify(&mut self) -> usize {
        self.end_region();
        let mut cells = 0;
        let mut hist: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        let hist_on = std::env::var_os("CLOISON_HEAP_HIST").is_some();
        // Small blocks by the share of them old cells fill, in tenths.
        let mut fill = [0usize; 11];
        let mut block = (0, 0);
        for (start, end) in self.block_ranges() {
            let large = end - start == GRANULE;
            if hist_on && !large && start & !(BLOCK - 1) != block.0 {
                if block.0 != 0 {
                    fill[block.1 * 10 / BLOCK] += 1;
                }
                block = (start & !(BLOCK - 1), 0);
            }
            let mut p = start;
            while p < end {
                let m = unsafe { *self.meta(p) };
                if m & (YOUNG | COLOR) == 0 {
                    assert_eq!(m, 0, "stray metadata at {p:#x}");
                    p += GRANULE;
                    continue;
                }
                let n = unsafe { cell_size(p as *const u8) };
                assert!(n >= GRANULE && (large || p + n <= end), "cell at {p:#x} of {n} bytes overruns {end:#x}");
                for g in (p + GRANULE..p + n - GRANULE).step_by(GRANULE) {
                    assert_eq!(unsafe { *self.meta(g) }, 0, "cell at {p:#x} overlaps one at {g:#x}");
                }
                let last = unsafe { *self.meta(p + n - GRANULE) };
                if m & COLOR != 0 {
                    assert!(last & END != 0, "old cell at {p:#x} without its end");
                } else if n > GRANULE {
                    assert_eq!(last, 0, "young cell at {p:#x} overlaps one");
                }
                if hist_on && !large && m & COLOR != 0 {
                    block.1 += n;
                }
                if hist_on {
                    let w = unsafe { *(p as *const u64) };
                    let k = if w & 1 == 0 {
                        format!("{:?}", unsafe { (*(p as *const Object)).class })
                    } else {
                        format!("kind{}", w & KIND)
                    };
                    let e = hist.entry(format!("{k} {n}")).or_default();
                    e.0 += 1;
                    e.1 += n;
                }
                p += n;
                cells += 1;
            }
        }
        if block.0 != 0 {
            fill[block.1 * 10 / BLOCK] += 1;
        }
        if hist_on {
            eprintln!("small blocks by old cells' fill, tenths: {fill:?}");
        }
        if !hist.is_empty() {
            let mut v: Vec<_> = hist.into_iter().collect();
            v.sort_by_key(|e| std::cmp::Reverse(e.1.1));
            for (k, (c, b)) in v.iter().take(25) {
                eprintln!("{k:>20} {c:>10} {:>8} MiB", b >> 20);
            }
        }
        cells
    }
}

/// Bytes of an object with `words` words after its header.
#[inline(always)]
pub fn object_bytes(words: usize) -> usize {
    (INLINE_BASE + 8 * words).next_multiple_of(GRANULE)
}

/// The size of the cell at `p`, from its first words.
///
/// # Safety
/// `p` must be a cell, its header written.
pub unsafe fn cell_size(p: *const u8) -> usize {
    unsafe {
        let w = *(p as *const u64);
        if w & 1 == 0 {
            let o = p as *const Object;
            let mut words = Object::inline_cap(o);
            match (*o).class {
                Class::String => words += (*o).elems_cap as usize,
                Class::Function => words += (&(*(*o).code).upvalues).len(),
                _ => {}
            }
            return object_bytes(words);
        }
        let n = match w & KIND {
            VALUES | WEAK_VALUES => 8 + 8 * (w >> 32) as usize,
            BYTES => 8 + (w >> 32) as usize,
            UPVALUE => 24,
            _ => panic!("no cell at {p:?}: header word {w:#x}"),
        };
        n.next_multiple_of(GRANULE)
    }
}

const _: () = assert!(BLOCK.is_multiple_of(CARD) && GRANULES_PER_BLOCK == 2048);

#[cfg(test)]
mod tests {
    use super::*;

    /// More heaps alive at once than tests running in parallel on a large
    /// machine, each with a slice of its own and a cell in it.
    #[test]
    fn many_heaps_alive_at_once() {
        let heaps: Vec<Heap> = (0..100)
            .map(|_| {
                let mut heap = Heap::new();
                heap.alloc_bytes(8);
                heap
            })
            .collect();
        let mut bases: Vec<usize> = heaps.iter().map(|h| h.base).collect();
        bases.sort();
        bases.dedup();
        assert_eq!(bases.len(), heaps.len());
    }

    /// Small cells share blocks without crossing their ends, large ones get
    /// blocks of their own; every cell is zeroed, aligned and walkable.
    #[test]
    fn cells_and_blocks() {
        let mut heap = Heap::new();
        let mut seen = Vec::new();
        for i in 0..20_000 {
            let n = (i % 37 + 1) * 16;
            let p = heap.alloc_bytes(n - 8);
            let s = unsafe { std::slice::from_raw_parts_mut(p, n - 8) };
            assert!(s.iter().all(|&b| b == 0));
            s.fill(0xAB);
            let cell = p as usize - 8;
            assert_eq!(cell % GRANULE, 0);
            assert_eq!(cell / BLOCK, (cell + n - 1) / BLOCK, "a small cell crosses a block");
            seen.push(cell);
        }
        let big = heap.alloc_values(10_000);
        assert_eq!((big as usize - 8) % BLOCK, 0);
        assert_eq!(unsafe { *big.add(9_999) }, Value::UNDEFINED);
        let u = heap.alloc_upvalue();
        assert_eq!(u as usize % GRANULE, 8);
        assert_eq!(heap.verify(), seen.len() + 2);
        assert!(seen.iter().all(|&c| heap.contains(c)));
        assert!(!heap.contains(&heap as *const Heap as usize));
    }
}
