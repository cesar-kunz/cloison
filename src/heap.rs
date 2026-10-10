//! The heap the collector manages (`docs/gc.md`).
//!
//! One reservation of address space, carved into 32 KiB blocks. A cell up
//! to [`LARGE`] bytes is allocated by bumping a pointer through a *hole*
//! (free granules) of a block, and never crosses a block's end; a larger
//! one gets a run of blocks of its own. Cells are 16-byte aligned and sized
//! in 16-byte granules, and never move.
//!
//! Beside the reservation, two side tables:
//!
//! - **Metadata**, one byte per granule. Allocation writes [`YOUNG`] at a
//!   cell's first granule; a collection that finds the cell alive replaces
//!   it with the mark color ([`COLOR`]) and writes [`END`] at the cell's
//!   last granule. So a granule's byte says whether a cell starts there and
//!   how old it is, without reading the cell: what a conservative root is
//!   checked against, and what holes are found from.
//! - **Cards**, one byte per [`CARD`] bytes, set by every store of a value
//!   into a cell (the write barrier, [`Heap::card_mark`]): what a minor
//!   collection scans old cells for young referents in.
//!
//! Every cell says how big it is from its first words ([`cell_size`]): an
//! object starts with its shape's address (8-aligned), anything else with
//! a header word whose low bit is set ([`VALUES`], [`BYTES`], [`UPVALUE`]).
//! Pointers to arrays of values (an object's `props` and `elems`) and to
//! upvalues point past that header word, at the payload.
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
/// Address space reserved for the heap (memory is used only as touched).
const RESERVE: usize = 32 << 30;

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

/// What a block holds.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Block {
    /// Cells of up to [`LARGE`] bytes.
    Small,
    /// The first of a large cell's `n` blocks.
    LargeHead(u32),
    LargeTail,
}

/// An anonymous mapping, unmapped on drop.
struct Mapping {
    ptr: *mut u8,
    len: usize,
}

impl Mapping {
    /// `len` bytes of zeroed memory, aligned to `align`, backed only as
    /// touched.
    fn new(len: usize, align: usize) -> Mapping {
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE;
        let all = len + align;
        let p = unsafe { libc::mmap(std::ptr::null_mut(), all, prot, flags, -1, 0) };
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
        Mapping {
            ptr: start as *mut u8,
            len,
        }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

#[repr(C)]
pub struct Heap {
    // Read by compiled code (`Heap::bump`): keep first, in this order.
    ptr: *mut u8,
    end: *mut u8,
    meta_bias: usize,
    card_bias: usize,
    /// Where the current hole started (allocations since are a region).
    hole: *mut u8,
    space: Mapping,
    meta: Mapping,
    cards: Mapping,
    /// Blocks handed out of the reservation so far.
    top: usize,
    blocks: Vec<Block>,
    /// The ranges cells were allocated in since the last collection, in
    /// order, the current hole's not yet (its end is `ptr`).
    regions: Vec<(usize, usize)>,
    /// Bytes handed out since the last collection, and in all.
    since: usize,
    total: usize,
}

impl Default for Heap {
    fn default() -> Self {
        Self::new()
    }
}

impl Heap {
    pub fn new() -> Heap {
        let reserve = std::env::var("TOI_HEAP_LIMIT")
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .map_or(RESERVE, |mb| (mb << 20).next_multiple_of(BLOCK));
        let space = Mapping::new(reserve, BLOCK);
        let meta = Mapping::new((reserve / GRANULE).next_multiple_of(PAGE), PAGE);
        let cards = Mapping::new((reserve / CARD).next_multiple_of(PAGE), PAGE);
        // Huge pages past the first few MiB, which small programs live in:
        // one fault per 2 MiB where the kernel has them.
        #[cfg(target_os = "linux")]
        unsafe {
            let skip = 4 << 20;
            if reserve > skip {
                libc::madvise(space.ptr.add(skip).cast(), reserve - skip, libc::MADV_HUGEPAGE);
            }
        }
        let base = space.ptr as usize;
        Heap {
            ptr: std::ptr::null_mut(),
            end: std::ptr::null_mut(),
            meta_bias: (meta.ptr as usize).wrapping_sub(base / GRANULE),
            card_bias: (cards.ptr as usize).wrapping_sub(base / CARD),
            hole: std::ptr::null_mut(),
            space,
            meta,
            cards,
            top: 0,
            blocks: Vec::new(),
            regions: Vec::new(),
            since: 0,
            total: 0,
        }
    }

    /// Where compiled code finds the free space: `ptr`, `end`, then the
    /// metadata's bias (see the module docs).
    pub(crate) fn bump(&mut self) -> *mut [*mut u64; 2] {
        (&raw mut self.ptr).cast()
    }

    /// Whether `a` is an address in the heap's reservation.
    #[inline(always)]
    pub fn contains(&self, a: usize) -> bool {
        a.wrapping_sub(self.space.ptr as usize) < self.space.len
    }

    /// The metadata byte of the granule at `a` (in the heap).
    #[inline(always)]
    pub fn meta(&self, a: usize) -> *mut u8 {
        self.meta_bias.wrapping_add(a / GRANULE) as *mut u8
    }

    /// The write barrier: a value was stored at address `a` (in a cell).
    #[inline(always)]
    pub fn card_mark(&self, a: *const Value) {
        unsafe { *(self.card_bias.wrapping_add(a as usize / CARD) as *mut u8) = 1 };
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
        unsafe { *self.meta(p as usize) = YOUNG };
        p
    }

    #[cold]
    #[inline(never)]
    fn alloc_slow(&mut self, bytes: usize) -> *mut u8 {
        if bytes > LARGE {
            return self.alloc_large(bytes);
        }
        self.end_region();
        let b = self.fresh_blocks(1);
        self.blocks[b] = Block::Small;
        let start = self.block_addr(b);
        self.hole = start;
        self.ptr = start;
        self.end = unsafe { start.add(BLOCK) };
        self.since += BLOCK;
        self.total += BLOCK;
        self.alloc(bytes)
    }

    /// Record the current hole's allocations as a region.
    fn end_region(&mut self) {
        if self.ptr > self.hole {
            self.regions.push((self.hole as usize, self.ptr as usize));
        }
        self.hole = self.ptr;
    }

    /// A large cell: blocks of its own.
    fn alloc_large(&mut self, bytes: usize) -> *mut u8 {
        let n = bytes.div_ceil(BLOCK);
        let b = self.fresh_blocks(n);
        self.blocks[b] = Block::LargeHead(n as u32);
        for k in 1..n {
            self.blocks[b + k] = Block::LargeTail;
        }
        let p = self.block_addr(b);
        self.regions.push((p as usize, p as usize + bytes));
        self.since += n * BLOCK;
        self.total += n * BLOCK;
        unsafe { *self.meta(p as usize) = YOUNG };
        p
    }

    /// `n` contiguous blocks never used before: the first's index.
    fn fresh_blocks(&mut self, n: usize) -> usize {
        let b = self.top;
        assert!(
            (b + n) * BLOCK <= self.space.len,
            "out of memory: the heap is limited to {} MiB (TOI_HEAP_LIMIT)",
            self.space.len >> 20
        );
        self.top += n;
        self.blocks.resize(self.top, Block::Small);
        b
    }

    fn block_addr(&self, b: usize) -> *mut u8 {
        unsafe { self.space.ptr.add(b * BLOCK) }
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

    /// Bytes of blocks handed out, in all.
    pub fn allocated(&self) -> usize {
        self.total
    }

    /// Walk every cell allocated since the last collection, checking that
    /// each says how big it is and has its metadata (for tests). With
    /// `TOI_HEAP_HIST` set, print the cells by kind and size.
    pub fn verify(&mut self) -> usize {
        self.end_region();
        let mut cells = 0;
        let mut hist: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        let mut used = 0;
        let hist_on = std::env::var_os("TOI_HEAP_HIST").is_some();
        for &(start, end) in &self.regions {
            used += end - start;
            let mut p = start;
            while p < end {
                assert_eq!(unsafe { *self.meta(p) }, YOUNG, "no cell starts at {p:#x}");
                let n = unsafe { cell_size(p as *const u8) };
                assert!(n >= GRANULE && p + n <= end, "cell at {p:#x} of {n} bytes overruns {end:#x}");
                for g in (p + GRANULE..p + n).step_by(GRANULE) {
                    assert_eq!(unsafe { *self.meta(g) }, 0, "cell at {p:#x} overlaps one at {g:#x}");
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
            eprintln!("used {} MiB in regions, {} MiB of blocks", used >> 20, self.total >> 20);
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
