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
//! - **Cards**, one byte per [`CARD`] bytes, set by every store of a value
//!   into a cell: what a minor collection scans old cells for young
//!   referents in.
//!
//! Every cell says how big it is from its first words ([`cell_size`]): an
//! object starts with its shape's address (8-aligned), anything else with
//! a header word whose low bit is set ([`VALUES`], [`BYTES`], [`UPVALUE`]).
//! Pointers to arrays of values (an object's `props` and `elems`) and to
//! upvalues point past that header word, at the payload.
//!
//! A hole's memory is zeroed when allocation takes it (blocks never used
//! are zero already): the `create_object` stencils write a header and
//! leave the slots.
//!
//! Compiled code allocates too, bumping `ptr` while it stays within `end`
//! and writing the metadata byte at `meta_bias + (cell >> 4)`: they come
//! first, in that order (see [`Heap::bump`]).

use crate::object::{Class, INLINE_BASE, Object, Shape, object_header};
use crate::value::Value;

/// Allocation granule: cell addresses and sizes are multiples of it.
pub const GRANULE: usize = 16;
/// Block size: allocation's unit of memory.
pub const BLOCK: usize = 32 << 10;
const GRANULES_PER_BLOCK: usize = BLOCK / GRANULE;
/// Bytes per card.
pub const CARD: usize = 512;
/// Cells larger than this get blocks of their own.
pub const LARGE: usize = 8 << 10;
/// Address space all heaps share: room for 512 heaps of a [`SLICE`] each.
/// Only reserved: no page is touched until committed.
const SPACE: usize = 16 << 40;
/// Address space one heap may use (`TOI_HEAP_LIMIT`, in MiB, lowers it).
const SLICE: usize = 32 << 30;
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

/// Metadata byte of a cell's first granule: allocated since the last
/// collection.
pub const YOUNG: u8 = 0x80;
/// Metadata bits of an old cell's first granule: the color it was marked
/// with (never 0).
pub const COLOR: u8 = 0x03;
/// Metadata bit of an old cell's last granule.
pub const END: u8 = 0x04;

/// Header word of an array of values: `VALUES | capacity << 32`.
pub const VALUES: u64 = 1;
/// Header word of a byte buffer: `BYTES | length << 32`.
pub const BYTES: u64 = 3;
/// Header word of an upvalue (`vm::Upvalue` follows).
pub const UPVALUE: u64 = 5;
const KIND: u64 = 7;

/// Holes smaller than this are left unused until the next collection.
fn min_hole() -> usize {
    static N: std::sync::OnceLock<usize> = std::sync::OnceLock::new();
    *N.get_or_init(|| std::env::var("TOI_GC_MIN_HOLE").ok().and_then(|s| s.parse().ok()).unwrap_or(256))
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
}

/// The reservation all heaps share, and its side tables.
struct Space {
    base: usize,
    /// Slices no heap has, by index.
    free: std::sync::Mutex<Vec<usize>>,
}

static SPACE_: std::sync::OnceLock<Space> = std::sync::OnceLock::new();
static META_BIAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static CARD_BIAS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// `len` bytes of address space, aligned to `align`, inaccessible until
/// committed.
fn reserve(len: usize, align: usize) -> usize {
    let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
    let all = len + align;
    let p = unsafe { libc::mmap(std::ptr::null_mut(), all, libc::PROT_NONE, flags, -1, 0) };
    assert!(p != libc::MAP_FAILED, "heap reservation failed");
    let start = (p as usize).next_multiple_of(align);
    unsafe {
        if start > p as usize {
            libc::munmap(p, start - p as usize);
        }
        let tail = p as usize + all - (start + len);
        if tail > 0 {
            libc::munmap((start + len) as *mut libc::c_void, tail);
        }
    }
    start
}

fn space() -> &'static Space {
    SPACE_.get_or_init(|| {
        let base = reserve(SPACE, CHUNK);
        let meta = reserve(SPACE / GRANULE, PAGE);
        let cards = reserve(SPACE / CARD, PAGE);
        let meta_bias = meta.wrapping_sub(base / GRANULE);
        assert!(meta_bias >> 48 == 0, "metadata below the heap's reservation / 16");
        META_BIAS.store(meta_bias, std::sync::atomic::Ordering::Relaxed);
        let card_bias = cards.wrapping_sub(base / CARD);
        // Compiled code holds it in a 48-bit immediate.
        assert!(card_bias >> 48 == 0, "card table below the heap's reservation / 512");
        CARD_BIAS.store(card_bias, std::sync::atomic::Ordering::Relaxed);
        Space {
            base,
            free: std::sync::Mutex::new((0..SPACE / SLICE).rev().collect()),
        }
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

/// The metadata byte of the granule at `a` (in a heap).
#[inline(always)]
pub fn meta(a: usize) -> *mut u8 {
    META_BIAS.load(std::sync::atomic::Ordering::Relaxed).wrapping_add(a / GRANULE) as *mut u8
}

/// The card byte of address `a` (in a heap).
#[inline(always)]
pub fn card(a: usize) -> *mut u8 {
    CARD_BIAS.load(std::sync::atomic::Ordering::Relaxed).wrapping_add(a / CARD) as *mut u8
}

/// What compiled code adds to an address shifted right by 9 to get its
/// card (see `toi_hole_cards` in the stencils).
pub fn card_bias() -> usize {
    space();
    CARD_BIAS.load(std::sync::atomic::Ordering::Relaxed)
}

/// Mark the card of `a`. A minor collection scans the old cells' words on
/// dirty cards, and the whole of an object that starts on one: so for a
/// store to an object's in-object slot, or to its `props` or `elems`, `a`
/// may be the object's own address (what compiled code passes).
#[inline(always)]
pub fn card_mark<T>(a: *const T) {
    unsafe { *card(a as usize) = 1 };
}

/// The write barrier: `v` was stored at `a`, in the cell at `host`. Only a
/// reference from an old cell to a young one needs recording, for the
/// next minor collection: its card is marked.
#[inline(always)]
pub fn barrier<T>(host: *const u8, a: *const T, v: Value) {
    unsafe {
        if v.is_cell() && *meta(v.to_bits() as usize) == YOUNG && *meta(host as usize) != YOUNG {
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

/// What compiled code adds to an address shifted right by 4 to get its
/// metadata byte (see `toi_hole_meta` in the stencils).
pub fn meta_bias() -> usize {
    space();
    META_BIAS.load(std::sync::atomic::Ordering::Relaxed)
}

#[repr(C)]
pub struct Heap {
    // Read by compiled code (`Heap::bump`): keep first, in this order.
    ptr: *mut u8,
    end: *mut u8,
    meta_bias: usize,
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
    /// Blocks that hold nothing.
    free: Vec<u32>,
    /// The ranges cells were allocated in since the last collection, in
    /// order, the current hole's not yet (its end is `ptr`).
    pub(crate) regions: Vec<(usize, usize)>,
    /// Bytes handed out since the last collection, and blocks in use.
    since: usize,
    used: usize,
    /// Bytes handed out after which the next collection is requested.
    budget: usize,
    /// A collection is due at the next safepoint (`Vm::safepoint`).
    pub(crate) requested: bool,
    /// The color the latest collection marked with (`COLOR`).
    pub(crate) color: u8,
    pub(crate) gc: crate::gc::Gc,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for Heap {
    fn drop(&mut self) {
        decommit(self.base, self.committed);
        space().free.lock().unwrap().push(self.slice);
    }
}

impl Heap {
    pub fn new() -> Heap {
        let limit = std::env::var("TOI_HEAP_LIMIT")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map_or(SLICE, |mb| (mb << 20).clamp(CHUNK, SLICE));
        let s = space();
        // (The lock is released before the check: a panic holding it would
        // poison it for every heap made or dropped after.)
        let slice = s.free.lock().unwrap().pop();
        let slice = slice.unwrap_or_else(|| panic!("more than {} heaps alive at once", SPACE / SLICE));
        let gc = crate::gc::Gc::new();
        Heap {
            budget: gc.budget(),
            requested: false,
            color: 1,
            gc,
            ptr: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
            meta_bias: META_BIAS.load(std::sync::atomic::Ordering::Relaxed),
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
            regions: Vec::new(),
            since: 0,
            used: 0,
        }
    }

    /// Where compiled code finds the free space: `ptr`, `end`, then the
    /// metadata's bias (see the module docs).
    pub(crate) fn bump(&mut self) -> *mut [*mut u64; 2] {
        (&raw mut self.ptr).cast()
    }

    /// Whether `a` is an address in the heap.
    #[inline(always)]
    pub fn contains(&self, a: usize) -> bool {
        a.wrapping_sub(self.base) < self.top * BLOCK
    }

    /// The metadata byte of the granule at `a` (in the heap).
    #[inline(always)]
    pub fn meta(&self, a: usize) -> *mut u8 {
        meta(a)
    }

    /// A new cell of `bytes` (a multiple of [`GRANULE`]), zeroed, its
    /// metadata [`YOUNG`].
    #[inline]
    pub fn alloc(&mut self, bytes: usize) -> *mut u8 {
        debug_assert!(bytes.is_multiple_of(GRANULE) && bytes > 0);
        let p = self.ptr;
        if (self.end as usize - p as usize) < bytes {
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
            if let Some((s, e)) = self.next_hole() {
                // Smaller holes would send compiled code's inline
                // allocation to the runtime every few cells.
                if e - s >= bytes.max(min_hole()) {
                    unsafe { std::ptr::write_bytes(s as *mut u8, 0, e - s) };
                    break (s, e);
                }
                continue;
            }
            if let Some(b) = self.recycle.pop() {
                self.cursor = self.block_addr(b as usize) as usize;
                self.cursor_end = self.cursor + BLOCK;
                continue;
            }
            let b = match self.free.pop() {
                Some(b) => {
                    self.clean(b as usize, 1);
                    b as usize
                }
                None => self.fresh_blocks(1),
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
            self.requested = true;
        }
        self.alloc(bytes)
    }

    /// The next hole of the block being allocated in: a run of granules no
    /// cell covers.
    fn next_hole(&mut self) -> Option<(usize, usize)> {
        let mut p = self.cursor;
        while p < self.cursor_end {
            if unsafe { *meta(p) } & (YOUNG | COLOR) != 0 {
                p += unsafe { cell_size(p as *const u8) };
                continue;
            }
            let s = p;
            while p < self.cursor_end && unsafe { *meta(p) } & (YOUNG | COLOR) == 0 {
                p += GRANULE;
            }
            self.cursor = p;
            return Some((s, p));
        }
        self.cursor = self.cursor_end;
        None
    }

    /// A collection finished: count afresh, give the blocks allocated in
    /// since the last one to allocation again, and request the next one
    /// after `budget` bytes.
    pub(crate) fn collected(&mut self, budget: usize, full: bool) {
        self.end_region();
        if !full {
            // Once each: a block a second time would be allocated in again
            // over cells it holds since (holes of one block need not be
            // consecutive regions: a large cell's comes between).
            let mut bs: Vec<u32> = (self.regions.iter())
                .map(|&(s, _)| ((s - self.base) / BLOCK) as u32)
                .filter(|&b| self.blocks[b as usize] == Block::Small)
                .collect();
            bs.sort_unstable();
            bs.dedup();
            self.recycle.extend(bs.into_iter().rev());
        }
        self.regions.clear();
        // The block being allocated in is among them: start afresh.
        self.ptr = std::ptr::null_mut();
        self.end = std::ptr::null_mut();
        self.hole = std::ptr::null_mut();
        self.cursor = 0;
        self.cursor_end = 0;
        self.since = 0;
        self.budget = budget;
        self.requested = false;
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

    /// After a full collection: allocate in the blocks at `partly` again,
    /// and free those at `empty`, giving their memory back to the system.
    pub(crate) fn reset_blocks(&mut self, partly: &[usize], empty: &[usize]) {
        self.recycle = partly.iter().rev().map(|&a| ((a - self.base) / BLOCK) as u32).collect();
        for &a in empty {
            let b = (a - self.base) / BLOCK;
            self.blocks[b] = Block::Free;
            self.free.push(b as u32);
            self.used -= BLOCK;
            release(a, BLOCK);
        }
    }

    /// Free the large cell at `p` (its blocks become free).
    pub(crate) fn free_large(&mut self, p: usize) {
        let b = (p - self.base) / BLOCK;
        let Block::LargeHead(n) = self.blocks[b] else { panic!("no large cell at {p:#x}") };
        unsafe { *meta(p) = 0 };
        for k in b..b + n as usize {
            self.blocks[k] = Block::Free;
            self.free.push(k as u32);
        }
        self.used -= n as usize * BLOCK;
        release(p, n as usize * BLOCK);
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
                Block::LargeTail | Block::Free => {}
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
            Block::Free => None,
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
            self.requested = true;
        }
        unsafe { *meta(p as usize) = YOUNG };
        p
    }

    /// `n` contiguous free blocks, zeroed and taken off the free list: the
    /// first's index.
    fn free_run(&mut self, n: usize) -> Option<usize> {
        if self.free.len() < n {
            return None;
        }
        let mut run = 0;
        let b = (0..self.top).find(|&b| {
            run = if self.blocks[b] == Block::Free { run + 1 } else { 0 };
            run == n
        })? + 1
            - n;
        self.free.retain(|&k| !(b..b + n).contains(&(k as usize)));
        self.clean(b, n);
        Some(b)
    }

    /// Zero `n` blocks from `b` that held cells, with their metadata.
    fn clean(&mut self, b: usize, n: usize) {
        let a = self.block_addr(b);
        unsafe {
            std::ptr::write_bytes(a, 0, n * BLOCK);
            std::ptr::write_bytes(meta(a as usize), 0, n * BLOCK / GRANULE);
        }
    }

    /// `n` contiguous blocks never used before: the first's index.
    fn fresh_blocks(&mut self, n: usize) -> usize {
        let b = self.top;
        let end = (b + n) * BLOCK;
        assert!(
            end <= self.limit,
            "out of memory: the heap is limited to {} MiB (TOI_HEAP_LIMIT)",
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
        }
        o
    }

    /// Room for `n` values, all `undefined`: the payload of an array cell.
    pub fn alloc_values(&mut self, n: usize) -> *mut Value {
        let cap = u32::try_from(n).expect("array too large") as u64;
        let p = self.alloc((8 + 8 * n).next_multiple_of(GRANULE)).cast::<u64>();
        unsafe {
            *p = VALUES | cap << 32;
            let v = p.add(1).cast::<Value>();
            for i in 0..n {
                v.add(i).write(Value::UNDEFINED);
            }
            v
        }
    }

    /// `n` zero bytes (8-aligned): the payload of a byte buffer cell.
    pub fn alloc_bytes(&mut self, n: usize) -> *mut u8 {
        let len = u32::try_from(n).expect("buffer too large") as u64;
        let p = self.alloc((8 + n).next_multiple_of(GRANULE)).cast::<u64>();
        unsafe {
            *p = BYTES | len << 32;
            p.add(1).cast()
        }
    }

    /// Room for an upvalue (`vm::Upvalue`, two words), zero: the payload
    /// of an upvalue cell.
    pub fn alloc_upvalue(&mut self) -> *mut u8 {
        let p = self.alloc(32).cast::<u64>();
        unsafe {
            *p = UPVALUE;
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
        (self.top, self.recycle.len(), self.free.len(), large)
    }

    /// Walk every cell, checking that each says how big it is and that
    /// the metadata agrees: a first granule `YOUNG` or colored, an old
    /// cell's last granule `END`, nothing else (for tests). With
    /// `TOI_HEAP_HIST` set, print the cells by kind and size.
    pub fn verify(&mut self) -> usize {
        self.end_region();
        let mut cells = 0;
        let mut hist: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        let hist_on = std::env::var_os("TOI_HEAP_HIST").is_some();
        for (start, end) in self.block_ranges() {
            let large = end - start == GRANULE;
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
            VALUES => 8 + 8 * (w >> 32) as usize,
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
