//! The collector (`docs/gc.md`): marking from the roots over the heap of
//! `crate::heap`.
//!
//! A collection runs at a safepoint ([`Vm::safepoint`]) when the heap asks
//! for one. A **minor** collection marks the young cells reachable from
//! the roots, from the old cells on dirty cards, and from the cells Rust
//! structures took since the last collection, with the current color
//! (`heap::COLOR`): they are old from then on. The young cells left
//! unmarked are free, their memory holes for allocation. A **full**
//! collection marks every reachable cell with a new color, its roots
//! including every Rust structure's references, and frees the rest.
//!
//! A full collection's marking runs concurrently ([`Marker`]): a **start**
//! pause advances the color and does a minor collection that also shades
//! the old cells its roots, survivors and dirty cards refer to; a marker
//! thread marks from those while the mutator runs, every store of a cell
//! not yet marked into an old cell dirtying a card (`heap::barrier`);
//! minor collections meanwhile pause it and shade what they find for it;
//! when it runs out of work, the **final** pause marks from the roots and
//! the dirty cards again, everything not marked yet, and sweeps.
//! `CLOISON_GC_STW` marks full collections in one pause instead.
//!
//! Roots are scanned conservatively where their words aren't known to be
//! values: the VM stack (every word checked against the metadata) and the
//! native stack with the registers (interior pointers resolved to their
//! cell).
//!
//! `CLOISON_GC_VERIFY=<KiB>` collects every that many KiB of allocation (at
//! least a block's), checks every reference it follows, and fills freed
//! cells with [`POISON`], so that a missed root or barrier shows up as a
//! poisoned value on first use; `CLOISON_GC_STRESS` collects at every
//! safepoint, the same way. `CLOISON_GC_STATS` prints each collection.

use crate::heap::{BYTES, COLOR, END, GRANULE, Heap, UPVALUE, VALUES, WEAK_VALUES, YOUNG, cell_size};
use crate::object::{Class, INLINE_BASE, Object};
use crate::value::Value;
use crate::vm::Vm;

/// The byte freed cells are filled with under `CLOISON_GC_VERIFY`.
pub const POISON: u8 = 0xdd;
/// Bytes allocated between minor collections.
const NURSERY: usize = 32 << 20;
/// A full collection runs when the heap's blocks have grown, since the
/// last one, by this many times what it left marked less one, and at least
/// [`FULL_MIN`].
const GROWTH: usize = 2;
const FULL_MIN: usize = 64 << 20;

/// The collector's state, in the heap.
#[derive(Default)]
pub struct Gc {
    /// Cells marked and not yet traced.
    stack: Vec<usize>,
    /// Collect every that many bytes, checking references and poisoning
    /// what is freed (`CLOISON_GC_VERIFY`; 0: off).
    pub verify: usize,
    /// Collect at every safepoint (`CLOISON_GC_STRESS`).
    stress: bool,
    /// Print each collection (`CLOISON_GC_STATS`).
    stats: bool,
    /// Collections so far.
    pub count: usize,
    /// The native stack's base: the highest address roots may be at
    /// (`Vm::run` records it).
    pub stack_base: usize,
    /// Cells Rust structures refer to for good (compiled code's constants,
    /// call sites' closures), each an address in or at a cell.
    kept: Vec<usize>,
    kept_set: crate::jit::HashSet<usize>,
    /// Cells stored into Rust structures since the last collection (their
    /// first granules), and addresses in or at cells taken for good since
    /// (see [`Heap::keep`]).
    log: Vec<usize>,
    log_kept: Vec<usize>,
    /// Slots of Rust structures (call caches) a young cell was stored in
    /// since the last collection: what they hold then is a root.
    log_slots: Vec<usize>,
    /// The heap's blocks in use above which the next collection is full.
    full_at: usize,
    /// Every collection full (`CLOISON_GC_FULL`).
    always_full: bool,
    /// Full collections marked in one pause (`CLOISON_GC_STW`).
    stw: bool,
    /// Concurrent full collections back to back (`CLOISON_GC_CYCLES`).
    cycles: bool,
    /// A full collection is due for memory outside the heap (shared
    /// buffers' blocks: see `shared`), whose dead owners only it finds.
    pub(crate) external_full: bool,
    /// A concurrent full collection is marking.
    marking: bool,
    /// The [`CODE_EPOCH`] the current or last full marking began in: the
    /// codes it found live are marked with it or later.
    codes_from: u32,
    /// What code may take (see [`code_bytes`]) before a full collection
    /// looks for dead code: what the last found live, at least `code_min`
    /// ([`CODE_MIN`]; less for tests).
    pub(crate) code_room: usize,
    pub(crate) code_min: usize,
    /// [`CODE_BYTES`] when the current or last full collection began.
    code_at_start: usize,
    /// The marker thread is sweeping after a final pause.
    pub(crate) sweeping: bool,
    /// What the heap may grow by before the next full collection (see
    /// [`GROWTH`]).
    pub(crate) room: usize,
    /// The heap's blocks in use when the current cycle started, and what
    /// the last cycle grew them by while marking: the next one starts that
    /// much earlier, so the heap peaks where a stop-the-world collection
    /// would have let it.
    cycle_from: usize,
    lead: usize,
    /// Bytes the last full collection found live (for `CLOISON_GC_STATS`).
    live: usize,
    /// Dirty card runs, kept for the next collection's (see
    /// `Heap::scan_cards`).
    card_runs: Vec<(usize, usize)>,
    /// Threads that scan dirty cards (`CLOISON_GC_THREADS`; 1: on the
    /// collecting thread only).
    threads: usize,
    /// Bytes minor collections promoted since the last major one.
    promoted: usize,
    /// Old cells a minor collection during marking shaded for the marker.
    shade: Vec<usize>,
    /// The marker thread, once a cycle needed it.
    pub(crate) marker: Option<Marker>,
    /// Bytes allocated between minor collections (`CLOISON_GC_NURSERY`, MiB).
    nursery: usize,
    /// Print what each kind of root reaches (`CLOISON_GC_ROOTS`).
    roots_report: bool,
    /// After each minor collection's marking, look for an old cell that
    /// refers to an unmarked young one, a missed barrier
    /// (`CLOISON_GC_CHECK_BARRIER`, slow).
    check_barriers: bool,
}

impl Gc {
    pub(crate) fn new() -> Gc {
        let env = |k| std::env::var_os(k).is_some();
        let verify = std::env::var("CLOISON_GC_VERIFY")
            .ok()
            .map_or(0, |s| s.parse::<usize>().unwrap_or(256).max(1) << 10);
        Gc {
            codes_from: 0,
            code_room: CODE_MIN,
            code_at_start: 0,
            code_min: CODE_MIN,
            verify,
            stress: env("CLOISON_GC_STRESS"),
            stats: env("CLOISON_GC_STATS"),
            always_full: env("CLOISON_GC_FULL"),
            stw: env("CLOISON_GC_STW"),
            cycles: env("CLOISON_GC_CYCLES"),
            check_barriers: env("CLOISON_GC_CHECK_BARRIER"),
            roots_report: env("CLOISON_GC_ROOTS"),
            nursery: std::env::var("CLOISON_GC_NURSERY")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .map_or(NURSERY, |mb| mb << 20),
            full_at: FULL_MIN,
            threads: std::env::var("CLOISON_GC_THREADS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
                .max(1),
            ..Default::default()
        }
    }

    #[cold]
    #[inline(never)]
    fn report_room(&self) {
        eprintln!("  live {} MiB, room {} MiB, lead {} MiB", self.live >> 20, self.room >> 20, self.lead >> 20);
    }

    /// What the heap may grow by after a sweep before the next cycle
    /// starts: the room less the last cycle's lead, half the room and
    /// [`FULL_MIN`] at least (more cycles of a small heap cost more time
    /// than they save memory: EarleyBoyer 12% at half of 64 MiB).
    pub(crate) fn headroom(&self) -> usize {
        self.room.saturating_sub(self.lead).max(self.room / 2).max(FULL_MIN)
    }

    /// Bytes allocated before the next collection.
    pub(crate) fn budget(&self) -> usize {
        match self.verify {
            _ if self.stress => 0,
            0 => self.nursery,
            n => n,
        }
    }

    fn checking(&self) -> bool {
        self.verify > 0 || self.stress
    }
}

impl Heap {
    /// Whether the cell `v` is young.
    #[inline(always)]
    pub fn is_young(&self, v: Value) -> bool {
        v.is_cell() && unsafe { *self.meta(v.to_bits() as usize) } == YOUNG
    }

    /// A Rust structure that full collections enumerate (an inline cache,
    /// a shape, the string table) now holds `v`: if it is a young cell, it
    /// is a root of the next collection (minor ones don't enumerate those
    /// structures).
    #[inline(always)]
    pub fn note(&mut self, v: Value) {
        // (Prototype caches note the same holder over and over: TypeScript
        // logged 386,000 cells for 31,000 between two collections.)
        let a = v.to_bits() as usize;
        if self.is_young(v) && self.gc.log.last() != Some(&a) {
            self.gc.log.push(a);
        }
    }

    /// A young cell was stored in slot `slot` of a Rust structure that
    /// full collections enumerate (a call cache): what it holds at the
    /// next collection is a root of it.
    #[inline]
    pub fn note_slot(&mut self, slot: *const *mut Object) {
        self.gc.log_slots.push(slot as usize);
    }

    /// A Rust structure refers to the cell at or around address `a` for
    /// good: compiled code embedding it, a call site caching it.
    pub fn keep(&mut self, a: usize) {
        if self.contains(a) && self.gc.kept_set.insert(a) {
            self.gc.kept.push(a);
            self.gc.log_kept.push(a);
        }
    }
}

/// What a collection marks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    /// Young cells (a minor collection).
    Minor,
    /// Young cells, and old ones not marked with the color yet are shaded
    /// for the marker (a minor collection during a marking cycle, and the
    /// start pause).
    MinorShade,
    /// Old cells not marked yet; young ones are left (the marker).
    Old,
    /// Every cell not marked yet (a stop-the-world full collection, the
    /// final pause).
    Full,
}

/// How a collection marks.
#[derive(Clone, Copy)]
struct Mode {
    color: u8,
    kind: Kind,
    /// Checks every reference it follows.
    check: bool,
}

/// Full markings so far, over every VM: a code found live is marked with
/// the epoch then (see `vm::Code::mark`).
static CODE_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Totals over every VM's collections (`cloison` prints them with
/// `CLOISON_SIZES` set, `scripts/octane-memory.sh` reads them): pauses,
/// minor and full (a concurrent full collection's two pauses count as one
/// full), their nanoseconds and the longest; the bytes of blocks in use at
/// the start of a collection (where the heap peaks), and the most a full
/// collection found live.
pub static MINORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FULLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PAUSE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static MAX_PAUSE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HEAP_PEAK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static LIVE_PEAK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The least code taken (see `Gc::code_room`) that starts a full
/// collection.
const CODE_MIN: usize = 32 << 20;

std::thread_local! {
    /// Bytes this thread's VM put into code since its last full marking
    /// (loaded, inline caches made: see `Code::footprint`).
    static CODE_BYTES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Note `n` more bytes of code (see [`CODE_BYTES`]).
pub(crate) fn code_bytes(n: usize) {
    CODE_BYTES.with(|c| c.set(c.get() + n));
}

impl Gc {
    /// Code has grown past its room: time for a full collection.
    pub(crate) fn code_full(&self) -> bool {
        CODE_BYTES.with(|c| c.get()) > self.code_room
    }

    /// Code has grown past twice its room: a marking under way is
    /// finished, not left to the marker (see `Vm::collect`).
    fn code_outgrown(&self) -> bool {
        CODE_BYTES.with(|c| c.get()) > 2 * self.code_room
    }
}

pub(crate) fn code_epoch() -> u32 {
    CODE_EPOCH.load(std::sync::atomic::Ordering::Relaxed)
}

/// Code `c` found live in the marking under way.
#[inline(always)]
pub(crate) fn mark_code(c: *const crate::vm::Code) {
    unsafe { (*c).mark.store(code_epoch(), std::sync::atomic::Ordering::Relaxed) }
}

/// If the cell at `p` (header word `w`), marked, is a closure: its code
/// found live. (Not in [`trace`]: card scanning traces dead cells too.)
#[inline(always)]
unsafe fn mark_function_code(p: usize, w: u64) {
    unsafe {
        if w & 1 == 0 && (*(p as *const Object)).class == Class::Function {
            mark_code((*(p as *const Object)).code);
        }
    }
}

/// What tracing a cell does with each reference it finds.
trait Visit {
    /// The cell at `p`.
    unsafe fn cell(&mut self, p: usize);

    /// `v`, if it is a cell.
    #[inline(always)]
    unsafe fn value(&mut self, v: Value) {
        if v.is_cell() {
            unsafe { self.cell(v.to_bits() as usize) }
        }
    }

    /// The cell an array's or upvalue's payload pointer `q` points into
    /// (past its header word), if not null.
    #[inline(always)]
    unsafe fn payload(&mut self, q: usize) {
        if q != 0 {
            unsafe { self.cell(q - 8) }
        }
    }

}

/// Marking: the color and kind, the cells marked and not yet traced, the
/// old cells shaded for the marker (`Kind::MinorShade`), and the heap's
/// range for checks.
struct Marking<'a> {
    mode: Mode,
    stack: &'a mut Vec<usize>,
    shade: &'a mut Vec<usize>,
    heap: (usize, usize),
    /// Bytes traced.
    marked: usize,
}

impl Visit for Marking<'_> {
    #[inline(always)]
    unsafe fn cell(&mut self, p: usize) {
        let mode = self.mode;
        unsafe {
            if mode.check && !is_cell_in(self.heap, p) {
                bad_reference(p);
            }
            let m = crate::heap::meta(p);
            let b = *m;
            // An old cell keeps its `END` (one granule's has it here too);
            // the rest is written when the cell is traced (its header is in
            // cache then).
            match mode.kind {
                Kind::Minor => {
                    if b != YOUNG {
                        return;
                    }
                    *m = mode.color;
                    self.stack.push(p);
                }
                Kind::MinorShade => {
                    if b == YOUNG {
                        *m = mode.color;
                        self.stack.push(p);
                    } else if b & COLOR != 0 && b & COLOR != mode.color {
                        *m = (b & END) | mode.color;
                        self.shade.push(p);
                    }
                }
                Kind::Old => {
                    if b & YOUNG != 0 || b & COLOR == 0 || b & COLOR == mode.color {
                        return;
                    }
                    // (A byte store: the mutator never writes an old cell's
                    // metadata while the marker runs.)
                    *m = (b & END) | mode.color;
                    self.stack.push(p);
                }
                Kind::Full => {
                    if b & COLOR == mode.color {
                        return;
                    }
                    *m = (b & END) | mode.color;
                    self.stack.push(p);
                }
            }
        }
    }

}

/// Card scanning: the references on dirty cards pass through a ring,
/// their metadata prefetched on entering it. Old cells' references go
/// all over the heap, so each metadata read is a cache miss otherwise,
/// and old cells hold most of the references dirty cards do.
struct CardRefs<'a, V: Visit> {
    m: &'a mut V,
    ring: [usize; CARD_REFS],
    n: usize,
}

/// How many references [`CardRefs`] holds (16: cards.js's longest pause
/// 19 ms against 16 ms; 64: no better).
const CARD_REFS: usize = 32;

impl<'a, V: Visit> CardRefs<'a, V> {
    fn new(m: &'a mut V) -> Self {
        CardRefs { m, ring: [0; CARD_REFS], n: 0 }
    }

    /// Mark what is still in the ring.
    unsafe fn flush(&mut self) {
        for i in self.n.saturating_sub(CARD_REFS)..self.n {
            unsafe { self.m.cell(self.ring[i % CARD_REFS]) }
        }
        self.n = 0;
    }
}

impl<V: Visit> Visit for CardRefs<'_, V> {
    #[inline(always)]
    unsafe fn cell(&mut self, p: usize) {
        prefetch(crate::heap::meta(p) as usize);
        let slot = self.n % CARD_REFS;
        if self.n >= CARD_REFS {
            unsafe { self.m.cell(self.ring[slot]) }
        }
        self.ring[slot] = p;
        self.n += 1;
    }
}

/// One thread's marking when a collection's dirty cards are scanned by
/// several (see [`Heap::scan_cards`]), as [`Marking`]'s, but a cell is
/// claimed by the thread whose compare-and-swap of its metadata byte
/// succeeds, so each lands on one stack. (One that fails lost to another
/// thread marking the cell with the same color: nothing left to do.)
struct CardMarking {
    mode: Mode,
    heap: (usize, usize),
    stack: Vec<usize>,
    shade: Vec<usize>,
}

impl Visit for CardMarking {
    #[inline(always)]
    unsafe fn cell(&mut self, p: usize) {
        use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
        let mode = self.mode;
        unsafe {
            if mode.check && !is_cell_in(self.heap, p) {
                bad_reference(p);
            }
            let m = &*(crate::heap::meta(p) as *const AtomicU8);
            let b = m.load(Relaxed);
            let (new, shade) = match mode.kind {
                Kind::Minor if b == YOUNG => (mode.color, false),
                Kind::MinorShade if b == YOUNG => (mode.color, false),
                Kind::MinorShade if b & COLOR != 0 && b & COLOR != mode.color => ((b & END) | mode.color, true),
                Kind::Full if b & COLOR != mode.color => ((b & END) | mode.color, false),
                _ => return,
            };
            if m.compare_exchange(b, new, Relaxed, Relaxed).is_ok() {
                if shade {
                    self.shade.push(p);
                } else {
                    self.stack.push(p);
                }
            }
        }
    }

}

/// A heap shared by the card-scanning threads, which only read it (but
/// for the young cells' metadata, claimed atomically).
#[derive(Clone, Copy)]
struct SharedHeap(*const Heap);
unsafe impl Send for SharedHeap {}
unsafe impl Sync for SharedHeap {}

/// Dirty cards (at [`heap::CARD`](crate::heap::CARD) bytes) from which a
/// minor collection scans them on several threads, and the most cards a
/// thread takes at a time. (Spawning the threads costs some tens of
/// microseconds.)
const PARALLEL_CARDS: usize = 4096;
const CARDS_PER_TAKE: usize = 64;
const RUNS_PER_TAKE: usize = 256;

impl Marking<'_> {
    /// Trace every marked cell until none is left; returns how many there
    /// were. `stop` is asked every few hundred cells' worth of work
    /// whether to stop early (the marker, asked to pause), leaving the rest
    /// on the stack.
    ///
    /// An array cell of more than [`SLICE_SLOTS`] values is traced that
    /// many at a time, the rest left on the stack as a pair: the next
    /// slot's index, then the cell's address with bit 0 set (cells are
    /// granule-aligned). One traced at once kept the marker from pausing
    /// for as long as it took (16 ms for fragmentation.js's table of 1.5
    /// million slots), and the pause waited for it.
    unsafe fn drain(&mut self, stop: &dyn Fn() -> bool) -> usize {
        unsafe { self.drain_to(stop, usize::MAX) }
    }

    /// [`drain`](Self::drain), stopping early also once it has traced
    /// `after` cells and the stack holds enough to share (for
    /// [`drain_parallel`](Self::drain_parallel)). By work done, not the
    /// stack's size: tracing a tree depth first keeps the stack about as
    /// deep as the tree.
    unsafe fn drain_to(&mut self, stop: &dyn Fn() -> bool, after: usize) -> usize {
        let mut n = 0;
        // Work since `stop` was last asked, in cells (a slice counts as
        // one per 32 slots).
        let mut work = 0;
        // Cells pass through a small ring between the stack and tracing,
        // prefetched as they enter it: by the time one is traced its
        // header has had `RING - 1` other cells' time to arrive. With the
        // slot a slice starts at (0: the whole cell, or its first slice).
        const RING: usize = 8;
        let mut ring = [(0usize, 0usize); RING];
        let (mut head, mut len) = (0, 0);
        unsafe {
            loop {
                while len < RING {
                    let Some(p) = self.stack.pop() else { break };
                    let entry = if p & 1 != 0 {
                        let from = self.stack.pop().expect("a slice's index");
                        let p = p & !1;
                        prefetch(p + 8 + 8 * from);
                        (p, from)
                    } else {
                        prefetch(p);
                        (p, 0)
                    };
                    ring[(head + len) % RING] = entry;
                    len += 1;
                }
                if len == 0 {
                    break;
                }
                let (p, from) = ring[head];
                head = (head + 1) % RING;
                len -= 1;
                let w = *(p as *const u64);
                let slots = if w & 7 == VALUES { (w >> 32) as usize } else { 0 };
                if from == 0 {
                    mark_function_code(p, w);
                    let size = cell_size(p as *const u8);
                    *crate::heap::meta(p + size - GRANULE) |= END;
                    self.marked += size;
                    n += 1;
                }
                if slots > SLICE_SLOTS {
                    let to = (from + SLICE_SLOTS).min(slots);
                    trace(self, p, p + 8 + 8 * from, p + 8 + 8 * to);
                    if to < slots {
                        self.stack.push(to);
                        self.stack.push(p | 1);
                    }
                    work += SLICE_SLOTS / 32;
                } else {
                    trace(self, p, 0, usize::MAX);
                    work += 1;
                }
                if work >= 256 {
                    work = 0;
                    if stop() {
                        break;
                    }
                }
                if n >= after && self.stack.len() >= SHARE_AT {
                    break;
                }
            }
            // Stopped early: what the ring holds goes back on the stack.
            for i in 0..len {
                match ring[(head + i) % RING] {
                    (p, 0) => self.stack.push(p),
                    (p, from) => {
                        self.stack.push(from);
                        self.stack.push(p | 1);
                    }
                }
            }
        }
        n
    }

    /// [`drain`](Self::drain) in a pause, on `threads` threads: each traces
    /// from its own stack, claiming the cells it marks by a
    /// compare-and-swap of their metadata ([`CardMarking`]), and hands
    /// half its stack to the shared queue whenever that is empty; done once
    /// every thread is out of work. Returns how many cells were traced.
    unsafe fn drain_parallel(&mut self, threads: usize) -> usize {
        // The stack's cells and array slices (see `drain`), the cells
        // dealt out, the slices to the first thread.
        let mut cells = Vec::with_capacity(self.stack.len());
        let mut slices = Vec::new();
        while let Some(p) = self.stack.pop() {
            if p & 1 != 0 {
                slices.push((p & !1, self.stack.pop().expect("a slice's index")));
            } else {
                cells.push(p);
            }
        }
        let pool = TracePool {
            state: std::sync::Mutex::new((Vec::new(), 0)),
            wake: std::sync::Condvar::new(),
            queued: std::sync::atomic::AtomicUsize::new(0),
            threads,
        };
        let per = cells.len().div_ceil(threads).max(1);
        let mut deal: Vec<Vec<usize>> = cells.chunks(per).map(<[usize]>::to_vec).collect();
        deal.resize_with(threads, Vec::new);
        let (mode, heap) = (self.mode, self.heap);
        let pool = &pool;
        let results: Vec<(usize, usize, Vec<usize>)> = std::thread::scope(|sc| {
            let mut deal = deal.into_iter();
            let first = (deal.next().unwrap(), slices);
            let helpers: Vec<_> = deal
                .map(|stack| sc.spawn(move || unsafe { trace_shared(pool, mode, heap, stack, Vec::new()) }))
                .collect();
            let mut r = vec![unsafe { trace_shared(pool, mode, heap, first.0, first.1) }];
            r.extend(helpers.into_iter().map(|h| h.join().unwrap()));
            r
        });
        let mut n = 0;
        for (cells, bytes, shade) in results {
            n += cells;
            self.marked += bytes;
            self.shade.extend(shade);
        }
        n
    }

    /// Mark the cell that word `w` points to the start of, if any (a word
    /// of the VM stack, a pending argument: values, or stale ones).
    #[inline(always)]
    unsafe fn exact(&mut self, w: usize) {
        if Value::from_bits(w as u64).is_cell() && is_cell_in(self.heap, w) {
            let check = std::mem::replace(&mut self.mode.check, false);
            unsafe { self.cell(w) };
            self.mode.check = check;
        }
    }
}

/// The work [`Marking::drain_parallel`]'s threads share: stacks handed
/// over, and how many threads are out of work (with the stacks, under the
/// lock); `queued` counts the stacks for a look without it.
struct TracePool {
    state: std::sync::Mutex<(Vec<Vec<usize>>, usize)>,
    wake: std::sync::Condvar,
    queued: std::sync::atomic::AtomicUsize,
    threads: usize,
}

impl TracePool {
    fn give(&self, stack: Vec<usize>) {
        let mut st = self.state.lock().unwrap();
        st.0.push(stack);
        self.queued.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.wake.notify_one();
    }

    /// A stack to trace from, waiting for one; `None` once every thread
    /// is out of work.
    fn take(&self) -> Option<Vec<usize>> {
        let mut st = self.state.lock().unwrap();
        st.1 += 1;
        loop {
            if let Some(stack) = st.0.pop() {
                st.1 -= 1;
                self.queued.fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
                return Some(stack);
            }
            if st.1 == self.threads {
                self.wake.notify_all();
                return None;
            }
            st = self.wake.wait(st).unwrap();
        }
    }
}

/// One of [`Marking::drain_parallel`]'s threads: trace from `stack` and
/// `slices`, then from what the others hand over, until all are out of
/// work. Returns the cells and bytes traced, and the old cells shaded.
unsafe fn trace_shared(
    pool: &TracePool,
    mode: Mode,
    heap: (usize, usize),
    stack: Vec<usize>,
    mut slices: Vec<(usize, usize)>,
) -> (usize, usize, Vec<usize>) {
    use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
    let mut cm = CardMarking { mode, heap, stack, shade: Vec::new() };
    let (mut n, mut bytes) = (0, 0);
    // As `drain`'s ring: cells prefetched `RING - 1` cells ahead.
    const RING: usize = 8;
    let mut ring = [0usize; RING];
    let (mut head, mut len) = (0, 0);
    unsafe {
        loop {
            loop {
                while len < RING {
                    let Some(p) = cm.stack.pop() else { break };
                    prefetch(p);
                    ring[(head + len) % RING] = p;
                    len += 1;
                }
                if len > 0 {
                    let p = ring[head];
                    head = (head + 1) % RING;
                    len -= 1;
                    let size = cell_size(p as *const u8);
                    (*(crate::heap::meta(p + size - GRANULE) as *const AtomicU8)).fetch_or(END, Relaxed);
                    bytes += size;
                    n += 1;
                    let w = *(p as *const u64);
                    mark_function_code(p, w);
                    let slots = if w & 7 == VALUES { (w >> 32) as usize } else { 0 };
                    if slots > SLICE_SLOTS {
                        trace(&mut cm, p, p + 8, p + 8 + 8 * SLICE_SLOTS);
                        slices.push((p, SLICE_SLOTS));
                    } else {
                        trace(&mut cm, p, 0, usize::MAX);
                    }
                } else if let Some((p, from)) = slices.pop() {
                    let slots = (*(p as *const u64) >> 32) as usize;
                    let to = (from + SLICE_SLOTS).min(slots);
                    trace(&mut cm, p, p + 8 + 8 * from, p + 8 + 8 * to);
                    if to < slots {
                        slices.push((p, to));
                    }
                } else {
                    break;
                }
                // Others out of work: half of this stack (its oldest
                // cells, the widest part of the graph) for them.
                if cm.stack.len() >= SHARE_AT && pool.queued.load(Relaxed) == 0 {
                    let rest = cm.stack.split_off(cm.stack.len() / 2);
                    pool.give(std::mem::replace(&mut cm.stack, rest));
                }
            }
            match pool.take() {
                Some(stack) => cm.stack = stack,
                None => break,
            }
        }
    }
    (n, bytes, cm.shade)
}

/// A pause traces on several threads once it has traced this many cells
/// on one, and a thread hands half its stack over from this many cells
/// (subtrees, for a tree traced depth first).
const PARALLEL_TRACE: usize = 16384;
const SHARE_AT: usize = 16;

/// The values of an array cell [`Marking::drain`] traces at a time.
const SLICE_SLOTS: usize = 1024;

/// Ask for the cache line at `p`.
#[inline(always)]
fn prefetch(p: usize) {
    #[cfg(target_arch = "aarch64")]
    unsafe {
        core::arch::asm!("prfm pldl1keep, [{0}]", in(reg) p, options(nostack, preserves_flags, readonly));
    }
    #[cfg(target_arch = "x86_64")]
    unsafe {
        core::arch::x86_64::_mm_prefetch::<{ core::arch::x86_64::_MM_HINT_T0 }>(p as *const i8);
    }
}

/// Whether there is a cell at `p` in the heap at `heap` (its first block
/// and its end), by the metadata: allocated since the last collection or
/// marked by one.
#[inline(always)]
fn is_cell_in(heap: (usize, usize), p: usize) -> bool {
    (heap.0..heap.1).contains(&p)
        && p.is_multiple_of(GRANULE)
        && unsafe { *crate::heap::meta(p) } & (YOUNG | COLOR) != 0
}

#[cold]
#[inline(never)]
fn bad_reference(p: usize) -> ! {
    panic!("reference to {p:#x}, which is no cell");
}

/// Visit what the cell at `p` refers to from its words in `lo..hi` (see
/// "Cells" in `docs/gc.md`).
///
/// # Safety
/// `p` must be a cell.
unsafe fn trace(v: &mut impl Visit, p: usize, lo: usize, hi: usize) {
    let within = |a: usize| (lo..hi).contains(&a);
    unsafe {
        let w = *(p as *const u64);
        if w & 1 != 0 {
            match w & 7 {
                VALUES => {
                    let n = (w >> 32) as usize;
                    let mut a = (p + 8).max(lo).next_multiple_of(8);
                    let last = (p + 8 + 8 * n).min(hi);
                    while a < last {
                        v.value(*(a as *const Value));
                        a += 8;
                    }
                }
                UPVALUE => {
                    if within(p + 16) {
                        v.value(*((p + 16) as *const Value));
                    }
                }
                // (Its header only: see `heap::WEAK_VALUES`.)
                WEAK_VALUES => {
                    let n = ((w >> 32) as usize).min(crate::collections::WEAK_HEADER);
                    let mut a = (p + 8).max(lo).next_multiple_of(8);
                    let last = (p + 8 + 8 * n).min(hi);
                    while a < last {
                        v.value(*(a as *const Value));
                        a += 8;
                    }
                }
                BYTES => {}
                _ => unreachable!(),
            }
            return;
        }
        let o = p as *mut Object;
        let cap = Object::inline_cap(o);
        let mut a = (p + INLINE_BASE).max(lo).next_multiple_of(8);
        let last = (p + INLINE_BASE + 8 * cap).min(hi);
        while a < last {
            v.value(*(a as *const Value));
            a += 8;
        }
        let props = &raw const (*o).props as usize;
        let elems = &raw const (*o).elems as usize;
        let code = &raw const (*o).code as usize;
        match (*o).class {
            Class::String => {
                if (*o).elems.is_null() {
                    // A cons string: its halves.
                    if within(props) {
                        v.value(Value::from_bits((*o).props as u64));
                    }
                    if within(code) {
                        v.value(Value::from_bits((*o).code as u64));
                    }
                } else if (*o).elems as usize != p + INLINE_BASE && within(elems) {
                    // Flattened: its units' buffer.
                    v.payload((*o).elems as usize);
                }
                return;
            }
            Class::Wrapper
            | Class::Map
            | Class::Set
            | Class::WeakMap
            | Class::WeakSet
            | Class::WeakRef
            | Class::FinalizationRegistry
            | Class::CollectionIterator
            | Class::Promise
            | Class::Symbol
            | Class::ArrayIterator
            | Class::IteratorHelper
            | Class::DisposableStack
            | Class::Generator => {
                // The primitive it wraps (a string is a cell), or a
                // collection's entries array, or a promise's state.
                if within(code) {
                    v.value(Value::from_bits((*o).code as u64));
                }
            }
            Class::TypedArray | Class::DataView => {
                // `elems` points into the buffer's bytes.
                if within(code) {
                    v.value(Value::from_bits((*o).code as u64));
                }
                if within(props) {
                    v.payload((*o).props as usize);
                }
                return;
            }
            Class::ArrayBuffer => {
                // A resizable one's views (see `typed`).
                if !(*o).code.is_null() && within(code) {
                    v.value(Value::from_bits((*o).code as u64));
                }
            }
            Class::SharedArrayBuffer => {
                // A growable one's views; its bytes are no cell.
                if !(*o).code.is_null() && within(code) {
                    v.value(Value::from_bits((*o).code as u64));
                }
                if within(props) {
                    v.payload((*o).props as usize);
                }
                return;
            }
            Class::Function => {
                // After the in-object slots it was allocated with, as
                // `cell_size` has it: the marker thread reads no shape.
                let ups = (&(*(*o).code).upvalues).len();
                let up = (p + INLINE_BASE + 8 * cap) as *const usize;
                debug_assert_eq!(up as usize, crate::vm::Vm::upvalues(o) as usize);
                for k in 0..ups {
                    if within(up.add(k) as usize) {
                        v.payload(*up.add(k));
                    }
                }
            }
            _ => {}
        }
        if within(props) {
            v.payload((*o).props as usize);
        }
        if within(elems) {
            v.payload((*o).elems as usize);
        }
    }
}

/// Run `f` with the callee-saved registers spilled into this frame, and
/// the stack pointer below them: every word a caller holds is then in
/// memory at or above it. (General registers only: arm64's callee-saved
/// `d8`-`d15` would hold a cell only if LLVM moved a pointer into a
/// floating-point register across a call, which it does not.)
#[inline(never)]
fn with_registers_spilled(f: &mut dyn FnMut(usize)) {
    let mut buf = [0usize; 12];
    let sp: usize;
    unsafe {
        #[cfg(target_arch = "aarch64")]
        core::arch::asm!(
            "stp x19, x20, [{b}, #0]",
            "stp x21, x22, [{b}, #16]",
            "stp x23, x24, [{b}, #32]",
            "stp x25, x26, [{b}, #48]",
            "stp x27, x28, [{b}, #64]",
            "stp x29, x30, [{b}, #80]",
            "mov {sp}, sp",
            b = in(reg) buf.as_mut_ptr(),
            sp = out(reg) sp,
            options(nostack),
        );
        #[cfg(target_arch = "x86_64")]
        core::arch::asm!(
            "mov [{b}], rbx",
            "mov [{b} + 8], rbp",
            "mov [{b} + 16], r12",
            "mov [{b} + 24], r13",
            "mov [{b} + 32], r14",
            "mov [{b} + 40], r15",
            "mov {sp}, rsp",
            b = in(reg) buf.as_mut_ptr(),
            sp = out(reg) sp,
            options(nostack),
        );
    }
    f(sp);
    std::hint::black_box(&buf);
}

/// The marker thread (see the module docs). It owns nothing of the VM:
/// it reads cells and writes old cells' metadata, from the work the
/// mutator hands it, and asks for the final pause when it runs out.
pub struct Marker {
    shared: std::sync::Arc<Shared>,
    thread: Option<std::thread::JoinHandle<()>>,
}

struct Shared {
    state: std::sync::Mutex<MarkerState>,
    wake: std::sync::Condvar,
    /// Asked to stop at the next check (the mutator wants a pause).
    pause: std::sync::atomic::AtomicBool,
}

#[derive(Default)]
struct MarkerState {
    /// Cells marked and not traced yet.
    work: Vec<usize>,
    color: u8,
    heap: (usize, usize),
    /// Marking (the mutator handed it work and hasn't paused it).
    run: bool,
    /// Tracing right now (with work of its own, off `work`).
    busy: bool,
    /// Ran out of work since the mutator last handed it some.
    done: bool,
    /// Where to ask for the final pause (`Heap::requested`).
    request: usize,
    quit: bool,
    /// Bytes traced in this cycle.
    marked: usize,
    /// Small blocks to sweep (their addresses), the color live cells have,
    /// whether to poison what is freed; and what sweeping them found:
    /// blocks with cells (and their free bytes), blocks with none.
    sweep: Vec<usize>,
    sweep_color: u8,
    poison: bool,
    swept: Vec<(usize, usize)>,
    emptied: Vec<usize>,
    /// Sweeping right now.
    sweeping: bool,
}

impl Marker {
    fn start() -> Marker {
        let shared = std::sync::Arc::new(Shared {
            state: Default::default(),
            wake: std::sync::Condvar::new(),
            pause: std::sync::atomic::AtomicBool::new(false),
        });
        let s = shared.clone();
        let thread = std::thread::Builder::new()
            .name("cloison-marker".into())
            .spawn(move || marker_loop(&s))
            .expect("the marker thread");
        Marker {
            shared,
            thread: Some(thread),
        }
    }

    /// Stop the marker and wait until it has: its work back in `work`.
    fn pause(&self) {
        let sh = &self.shared;
        sh.pause.store(true, std::sync::atomic::Ordering::Relaxed);
        let mut st = sh.state.lock().unwrap();
        while st.busy || st.sweeping {
            st = sh.wake.wait(st).unwrap();
        }
    }

    /// Let it go on (sweeping, say) after a pause that gave it no work.
    fn unpause(&self) {
        let sh = &self.shared;
        let _st = sh.state.lock().unwrap();
        sh.pause.store(false, std::sync::atomic::Ordering::Relaxed);
        sh.wake.notify_all();
    }

    /// Hand it `more` work and let it run.
    fn resume(&self, more: &mut Vec<usize>, request: usize) {
        let sh = &self.shared;
        let mut st = sh.state.lock().unwrap();
        st.work.append(more);
        st.request = request;
        // Nothing to trace (a start pause that shaded nothing): done, and
        // the next safepoint is the final pause, which the marker, waiting
        // for work, would never ask for.
        let done = st.work.is_empty() && !st.busy;
        st.run = !done;
        st.done = done;
        if done {
            unsafe { (*(request as *const std::sync::atomic::AtomicBool)).store(true, std::sync::atomic::Ordering::Relaxed) };
        }
        sh.pause.store(false, std::sync::atomic::Ordering::Relaxed);
        sh.wake.notify_all();
    }
}

impl Drop for Marker {
    fn drop(&mut self) {
        {
            let mut st = self.shared.state.lock().unwrap();
            st.quit = true;
            self.shared.pause.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        self.shared.wake.notify_all();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn marker_loop(sh: &Shared) {
    use std::sync::atomic::Ordering::Relaxed;
    let mut stack = Vec::new();
    let mut shade = Vec::new();
    loop {
        let (color, heap) = {
            let mut st = sh.state.lock().unwrap();
            loop {
                if st.quit {
                    return;
                }
                if !sh.pause.load(Relaxed) {
                    if st.run && !st.work.is_empty() {
                        break;
                    }
                    if !st.sweep.is_empty() {
                        // Sweep a few blocks, then look again.
                        let k = st.sweep.len().saturating_sub(64);
                        let blocks = st.sweep.split_off(k);
                        let (color, poison) = (st.sweep_color, st.poison);
                        st.sweeping = true;
                        drop(st);
                        let mut swept = Vec::with_capacity(blocks.len());
                        let mut emptied = Vec::new();
                        for &b in &blocks {
                            match unsafe { sweep_block(b, color, poison) } {
                                0 => emptied.push(b),
                                live => swept.push((b, crate::heap::BLOCK - live)),
                            }
                        }
                        st = sh.state.lock().unwrap();
                        st.swept.append(&mut swept);
                        st.emptied.append(&mut emptied);
                        st.sweeping = false;
                        sh.wake.notify_all();
                        continue;
                    }
                }
                st = sh.wake.wait(st).unwrap();
            }
            std::mem::swap(&mut stack, &mut st.work);
            st.busy = true;
            (st.color, st.heap)
        };
        let mut m = Marking {
            mode: Mode {
                color,
                kind: Kind::Old,
                check: false,
            },
            stack: &mut stack,
            shade: &mut shade,
            heap,
            marked: 0,
        };
        unsafe { m.drain(&|| sh.pause.load(Relaxed)) };
        let marked = m.marked;
        let mut st = sh.state.lock().unwrap();
        st.marked += marked;
        st.work.append(&mut stack);
        st.busy = false;
        if st.work.is_empty() && !sh.pause.load(Relaxed) {
            // Out of work: the mutator's next safepoint is the final pause.
            st.run = false;
            st.done = true;
            unsafe { (*(st.request as *const std::sync::atomic::AtomicBool)).store(true, Relaxed) };
        }
        sh.wake.notify_all();
    }
}

impl Vm {
    /// A safepoint: collect if the heap asks for it. `regs` is the running
    /// frame's registers (or null).
    #[inline(always)]
    pub(crate) fn safepoint(&mut self, regs: *mut Value) {
        if self.heap.requested.load(std::sync::atomic::Ordering::Relaxed) && self.heap.gc.stack_base != 0 {
            self.collect(regs);
        }
    }

    #[cold]
    #[inline(never)]
    fn collect(&mut self, regs: *mut Value) {
        let vm: *mut Vm = self;
        let external = !self.heap.gc.marking && std::mem::take(&mut self.heap.gc.external_full);
        let gc = &self.heap.gc;
        let kind = if gc.marking {
            // Cleared before `done` is read: a request the marker makes
            // after this is kept for the next safepoint.
            self.heap.requested.store(false, std::sync::atomic::Ordering::Relaxed);
            let done = gc.marker.as_ref().is_some_and(|m| m.shared.state.lock().unwrap().done);
            // The final pause: the marker is done, or the heap or code has
            // outgrown the cycle (the mutator then finishes the marking: a
            // marker short of time left code dead `eval`s made to pile up).
            if done || self.heap.allocated() > 2 * gc.full_at || gc.code_outgrown() {
                Collection::Final
            } else if self.heap.since() >= gc.budget() {
                Collection::MinorShade
            } else {
                // (The marker's request of the final pause came early.)
                return;
            }
        } else if gc.always_full
            || external
            || gc.cycles
            || self.heap.allocated() > gc.full_at
            || gc.promoted > gc.room.max(FULL_MIN)
            || gc.code_full()
        {
            if gc.stw || gc.always_full { Collection::Full } else { Collection::Start }
        } else {
            Collection::Minor
        };
        with_registers_spilled(&mut |sp| unsafe { (*vm).collect_from(sp, regs, kind) });
    }

    /// A collection, the native stack's live part starting at `sp`.
    fn collect_from(&mut self, sp: usize, regs: *mut Value, kind: Collection) {
        let start = std::time::Instant::now();
        let check = self.heap.gc.checking();
        HEAP_PEAK.fetch_max(self.heap.allocated() as u64, std::sync::atomic::Ordering::Relaxed);
        if let Some(m) = &self.heap.gc.marker {
            m.pause();
        }
        if matches!(kind, Collection::Start | Collection::Full) {
            self.heap.finish_sweep();
            // The cells that could still refer to them are swept.
            self.condemned.clear();
            self.heap.gc.codes_from = CODE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            self.heap.gc.code_at_start = CODE_BYTES.with(|c| c.get());
        } else if self.heap.gc.sweeping {
            self.heap.absorb_sweep();
        }
        if kind == Collection::Start {
            self.heap.gc.cycle_from = self.heap.allocated();
            self.heap.next_color();
            self.heap.gc.marking = true;
        }
        let color = if kind == Collection::Full { self.heap.next_color() } else { self.heap.color };
        let full = matches!(kind, Collection::Full | Collection::Final);
        let mode = Mode {
            color,
            kind: match kind {
                Collection::Minor => Kind::Minor,
                Collection::Start | Collection::MinorShade => Kind::MinorShade,
                Collection::Full | Collection::Final => Kind::Full,
            },
            check,
        };
        let (marked, freed);
        let mut times = [0f64; 4];
        let mut lap = {
            let mut t = std::time::Instant::now();
            move || {
                let now = std::time::Instant::now();
                let d = (now - t).as_secs_f64() * 1e3;
                t = now;
                d
            }
        };
        // The current hole's cells are young too.
        self.heap.end_region();
        let mut stack = std::mem::take(&mut self.heap.gc.stack);
        let mut shade = std::mem::take(&mut self.heap.gc.shade);
        // The final pause finishes the marker's work.
        if kind == Collection::Final
            && let Some(mk) = &self.heap.gc.marker
        {
            let mut st = mk.shared.state.lock().unwrap();
            stack.append(&mut st.work);
            st.run = false;
            st.done = false;
        }
        let range = self.heap.cells_range();
        let mut m = Marking {
            mode,
            stack: &mut stack,
            shade: &mut shade,
            heap: range,
            marked: 0,
        };
        unsafe {
            self.mark_roots(&mut m, sp, regs);
            times[0] = lap();
            if matches!(kind, Collection::Minor | Collection::MinorShade) {
                self.heap.scan_cards(&mut m);
            } else {
                // The start pause shades what Rust structures refer to; a
                // full marking marks it, and rescans what was stored into
                // old cells while the marker ran.
                self.mark_structures(&mut m);
                if kind != Collection::Full {
                    self.heap.scan_cards(&mut m);
                }
            }
            times[1] = lap();
            // On one thread until there is enough to share (a minor
            // collection's roots may lead to most of the nursery).
            let threads = self.heap.gc.threads;
            marked = if threads > 1 {
                let n = m.drain_to(&|| false, PARALLEL_TRACE);
                n + if m.stack.is_empty() { 0 } else { m.drain_parallel(threads) }
            } else {
                m.drain(&|| false)
            };
            // (The weak collections' entries, once what is live is known.)
            self.ephemerons(&mut m);
            if kind == Collection::Minor && self.heap.gc.check_barriers {
                self.heap.check_barriers();
            }
            if full {
                self.mark_live_codes(&mut m, sp, regs);
            }
            if kind == Collection::Final && check {
                self.verify_marking(sp, regs, color);
            }
            times[2] = lap();
            freed = match kind {
                // The young cells left unmarked are freed now, not by the
                // sweep: until their block is swept, a minor collection
                // would take a stale word to one for a live young cell and
                // keep it, with what it refers to swept from under it.
                Collection::Final => self.heap.free_young(check) + self.heap.start_sweep(color, check),
                Collection::Full => self.heap.free_unmarked(color, check),
                _ => self.heap.free_young(check),
            };
            if full {
                self.condemn_codes();
            }
            times[3] = lap();
        }
        let traced = m.marked;
        if full {
            self.heap.gc.promoted = 0;
        } else {
            self.heap.gc.promoted += traced;
        }
        self.heap.gc.stack = stack;
        self.heap.gc.log.clear();
        self.heap.gc.log_kept.clear();
        self.heap.gc.log_slots.clear();
        self.heap.gc.count += 1;
        if full {
            self.heap.gc.marking = false;
            let concurrent = self.heap.gc.marker.as_ref().map_or(0, |mk| {
                std::mem::take(&mut mk.shared.state.lock().unwrap().marked)
            });
            // The blocks the sweep left in use (partly free ones too),
            // and room to grow by what was live, times `GROWTH - 1`.
            let live = traced + concurrent;
            let room = ((GROWTH - 1) * live).max(FULL_MIN);
            LIVE_PEAK.fetch_max(live as u64, std::sync::atomic::Ordering::Relaxed);
            self.heap.gc.live = live;
            self.heap.gc.room = room;
            if kind == Collection::Final {
                self.heap.gc.lead = self.heap.allocated().saturating_sub(self.heap.gc.cycle_from);
            }
            // (After a final pause, again once the sweep has freed blocks.)
            self.heap.gc.full_at = self.heap.allocated() + self.heap.gc.headroom();
        }
        if self.heap.gc.stats {
            let (top, recycle, free, large) = self.heap.block_counts();
            eprintln!(
                "gc {} {:?}: {:.3} ms (roots {:.3}, {} {:.3}, trace {:.3}, free {:.3}), {marked} cells marked, \
                 {} shaded, {} KiB freed, {} MiB of blocks ({top} blocks, {recycle} to reuse, {free} free, \
                 {large} large), holes {:?}, to sweep {}",
                self.heap.gc.count,
                kind,
                start.elapsed().as_secs_f64() * 1e3,
                times[0],
                if full { "structures, cards" } else { "cards" },
                times[1],
                times[2],
                times[3],
                shade.len(),
                freed >> 10,
                self.heap.allocated() >> 20,
                self.heap.holes,
                self.heap.gc.marker.as_ref().map_or(0, |mk| mk.shared.state.lock().unwrap().sweep.len())
            );
            if full {
                self.heap.gc.report_room();
            }
        }
        {
            use std::sync::atomic::Ordering::Relaxed;
            let ns = start.elapsed().as_nanos() as u64;
            PAUSE_NS.fetch_add(ns, Relaxed);
            MAX_PAUSE_NS.fetch_max(ns, Relaxed);
            match kind {
                Collection::Minor | Collection::MinorShade => MINORS.fetch_add(1, Relaxed),
                Collection::Full | Collection::Final => FULLS.fetch_add(1, Relaxed),
                Collection::Start => 0,
            };
        }
        self.heap.holes = [0; 3];
        let next = self.heap.gc.budget();
        self.heap.collected(next, full);
        // Shaded cells are the marker's to trace.
        if self.heap.gc.marking {
            let request = &raw const self.heap.requested as usize;
            if self.heap.gc.marker.is_none() {
                self.heap.gc.marker = Some(Marker::start());
            }
            let mk = self.heap.gc.marker.as_ref().unwrap();
            {
                let mut st = mk.shared.state.lock().unwrap();
                st.color = color;
                st.heap = range;
            }
            mk.resume(&mut shade, request);
        } else if self.heap.gc.sweeping
            && let Some(mk) = &self.heap.gc.marker
        {
            mk.unpause();
        }
        self.heap.gc.shade = shade;
        if self.heap.gc.stress {
            self.heap.requested.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    /// The weak collections' ephemerons, once marking has found what the
    /// roots reach: the value of each entry whose key is live marked (and
    /// what that reaches, which may make other keys live, until none
    /// does), then the entries of keys not live cleared; collections not
    /// live leave `weak_tables`. A minor collection keeps old cells, and
    /// the keys an old collection's entries refer to were marked from its
    /// dirty cards: it clears young collections' entries only.
    unsafe fn ephemerons(&mut self, m: &mut Marking) {
        if self.weak_tables.is_empty() {
            return;
        }
        let mode = m.mode;
        let live = |p: usize| unsafe {
            let b = *crate::heap::meta(p);
            match mode.kind {
                Kind::Minor | Kind::MinorShade => b != YOUNG,
                _ => b & COLOR == mode.color,
            }
        };
        // The live collections, found as they become live: one may be
        // reached only through another's value (`m.set(k, new WeakMap)`).
        let mut tables: Vec<*mut Object> = Vec::new();
        let mut found = vec![false; self.weak_tables.len()];
        // The entries of each live collection: its buffer, its length, and
        // its entries' width.
        let entries = |t: *mut Object| unsafe {
            let e = Value::from_bits((*t).code as u64);
            if !e.is_cell() {
                return None;
            }
            let e = crate::object::as_object(e);
            let s = if matches!((*t).class, Class::WeakMap | Class::FinalizationRegistry) { 2 } else { 1 };
            Some(((*e).elems, (*e).elems_len as usize, s))
        };
        unsafe {
            loop {
                let mut more = false;
                for (j, &t) in self.weak_tables.iter().enumerate() {
                    if !found[j] && live(t as usize) {
                        found[j] = true;
                        tables.push(t);
                    }
                }
                for &t in &tables {
                    let Some((buf, len, 2)) = entries(t) else { continue };
                    let mut i = crate::collections::BASE;
                    while i + 1 < len {
                        let (k, v) = (*buf.add(i), *buf.add(i + 1));
                        if k.is_cell() && live(k.to_bits() as usize) && v.is_cell() && !live(v.to_bits() as usize) {
                            m.cell(v.to_bits() as usize);
                            more = true;
                        }
                        i += 2;
                    }
                }
                if !more {
                    break;
                }
                m.drain(&|| false);
            }
            for &t in &tables {
                let Some((buf, len, s)) = entries(t) else { continue };
                let mut i = crate::collections::BASE;
                while i + s <= len {
                    let k = *buf.add(i);
                    if k.is_cell() && !live(k.to_bits() as usize) {
                        for j in 0..s {
                            *buf.add(i + j) = Value::EMPTY;
                        }
                        let count = buf.add(crate::collections::COUNT);
                        *count = Value::from_i32((*count).as_i32_unchecked() - 1);
                    }
                    i += s;
                }
            }
        }
        self.weak_tables = tables;
    }

    /// Debugging (`CLOISON_GC_ROOTS`): trace what the roots marked so far
    /// reach, and print how many cells that was, for root kind `what`.
    unsafe fn roots_report(&mut self, m: &mut Marking, what: &str) {
        if !self.heap.gc.roots_report {
            return;
        }
        let n = unsafe { m.drain(&|| false) };
        eprintln!("  roots {what}: {n} cells");
    }

    /// The run is ending: pause the marker (its pointer to the heap's
    /// request flag would go stale if the VM moved); the next collection
    /// resumes it.
    pub(crate) fn pause_marker(&mut self) {
        if let Some(m) = &self.heap.gc.marker {
            m.pause();
        }
    }

    /// Mark the cell that word `w` points into, if any (a native stack
    /// word, a Rust structure's address of a slot).
    unsafe fn mark_interior(&self, m: &mut Marking, w: usize) {
        if let Some(p) = self.heap.cell_containing(w) {
            let check = std::mem::replace(&mut m.mode.check, false);
            unsafe { m.cell(p) };
            m.mode.check = check;
        }
    }

    /// Mark what code `code` refers to: its constants, and the cells its
    /// inline caches hold (a load's prototype, a call's callee; a store's
    /// is a field type, `FieldType::encode`, not in the heap).
    unsafe fn mark_code_refs(&self, m: &mut Marking, code: &crate::vm::Code) {
        unsafe {
            for &c in code.constants.iter() {
                m.value(c);
            }
            for ways in code.made_ics().iter() {
                for ic in (*ways.get()).iter() {
                    let h = ic.holder as usize;
                    if self.heap.contains(h) {
                        m.cell(h);
                    }
                }
            }
        }
    }

    /// A full marking's codes: live if pinned, on a stack (the native
    /// one, or the VM's, as a frame's code), or marked since it began (a
    /// closure of it traced or made, it loaded), and with them the
    /// functions nested in them, which they make closures of. Mark what
    /// the live ones refer to, and what that reaches, until no more are.
    unsafe fn mark_live_codes(&mut self, m: &mut Marking, sp: usize, regs: *mut Value) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        unsafe {
            let stack = self.stack.as_ptr() as usize;
            let stack_end = self.stack_end as usize;
            let top = (regs as usize).max(self.osr_regs as usize).max(self.native_top as usize);
            let top = (top + 8 * self.max_frame).clamp(stack, stack_end);
            for a in (sp..self.heap.gc.stack_base).step_by(8).chain((stack..top).step_by(8)) {
                let w = *(a as *const usize);
                if self.code_set.contains(&w) {
                    mark_code(w as *const crate::vm::Code);
                }
            }
        }
        let mut done = vec![false; self.codes.len()];
        loop {
            let mut more = false;
            for (k, code) in self.codes.iter().enumerate() {
                if done[k] || !(code.pinned.get() || code.mark.load(Relaxed) >= from) {
                    continue;
                }
                done[k] = true;
                more = true;
                for &c in &code.closures {
                    mark_code(c);
                }
                unsafe { self.mark_code_refs(m, code) };
            }
            if !more {
                break;
            }
            unsafe { m.drain(&|| false) };
        }
    }

    /// After a full marking, take the codes it didn't find live out of
    /// use: freed at the next collection that finishes the sweep first.
    fn condemn_codes(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        let (live, dead): (Vec<_>, Vec<_>) = std::mem::take(&mut self.codes)
            .into_iter()
            .partition(|c| c.pinned.get() || c.mark.load(Relaxed) >= from);
        self.codes = live;
        let live: usize = self.codes.iter().map(|c| c.footprint()).sum();
        // (Less the code made while the marker ran, all found live: its
        // room grew with each marking a slow marker drew out.)
        let made = CODE_BYTES.with(|c| c.get()).saturating_sub(self.heap.gc.code_at_start);
        self.heap.gc.code_room = live.saturating_sub(made).max(self.heap.gc.code_min);
        CODE_BYTES.with(|c| c.set(0));
        for c in &dead {
            self.code_set.remove(&(&**c as *const crate::vm::Code as usize));
        }
        self.condemned.extend(dead);
    }

    /// Mark the roots every collection has.
    unsafe fn mark_roots(&mut self, m: &mut Marking, sp: usize, regs: *mut Value) {
        unsafe {
            // The native stack, and the highest VM stack address it holds:
            // the running frame's registers are there or in `regs`.
            let stack = self.stack.as_ptr() as usize;
            let stack_end = self.stack_end as usize;
            let mut top = (regs as usize).max(self.osr_regs as usize).max(self.native_top as usize);
            let base = self.heap.gc.stack_base;
            for a in (sp..base).step_by(8) {
                let w = *(a as *const usize);
                if (stack..stack_end).contains(&w) {
                    top = top.max(w);
                } else {
                    self.mark_interior(m, w);
                }
            }
            self.roots_report(m, "native stack");
            // The VM stack, to the end of the deepest frame there may be.
            let top = (top + 8 * self.max_frame).clamp(stack, stack_end);
            for a in (stack..top).step_by(8) {
                m.exact(*(a as *const usize));
            }
            self.roots_report(m, &format!("vm stack ({} words)", (top - stack) / 8));
            let (args, n) = self.pending_args;
            if !args.is_null() {
                for i in 0..n {
                    m.exact((*args.add(i)).to_bits() as usize);
                }
            }
            // The VM's own references.
            for v in self.named_roots() {
                m.value(v);
            }
            self.roots_report(m, "named");
            for i in 0..self.handles.len() {
                m.value(self.handles[i]);
            }
            for i in 0..self.kept_alive.len() {
                m.value(self.kept_alive[i]);
            }
            for i in 0..self.open_upvalues.len() {
                m.payload(self.open_upvalues[i] as usize);
            }
            // What Rust structures took since the last collection: cells
            // (young ones, so still cells), and addresses in or at cells.
            for i in 0..self.heap.gc.log.len() {
                m.cell(self.heap.gc.log[i]);
            }
            for i in 0..self.heap.gc.log_kept.len() {
                let a = self.heap.gc.log_kept[i];
                self.mark_interior(m, a);
            }
            for i in 0..self.heap.gc.log_slots.len() {
                let h = *(self.heap.gc.log_slots[i] as *const usize);
                m.value(Value::from_bits(h as u64));
            }
            self.roots_report(m, "handles, upvalues, log");
        }
    }

    /// Mark what Rust structures refer to: shapes, code, compiled code, the
    /// VM's tables.
    unsafe fn mark_structures(&mut self, m: &mut Marking) {
        unsafe {
            for i in 0..self.heap.gc.kept.len() {
                let a = self.heap.gc.kept[i];
                self.mark_interior(m, a);
            }
            self.roots_report(m, "kept");
            for s in self.shapes() {
                m.value(s.proto);
            }
            // The other codes' references count once they are found
            // live (see `mark_live_codes`).
            for code in self.codes().filter(|c| c.pinned.get()) {
                self.mark_code_refs(m, code);
            }
            for v in self.string_constants() {
                m.value(v);
            }
            self.roots_report(m, "shapes, code, strings");
        }
    }

    /// Under `CLOISON_GC_VERIFY`, after the final pause marked: mark again from
    /// every root in one pass, with a set of its own, and panic on a cell
    /// reached that the concurrent marking left unmarked.
    unsafe fn verify_marking(&mut self, sp: usize, regs: *mut Value, color: u8) {
        struct Reach {
            seen: crate::jit::HashSet<usize>,
            stack: Vec<usize>,
            color: u8,
        }
        impl Visit for Reach {
            unsafe fn cell(&mut self, p: usize) {
                if self.seen.insert(p) {
                    let b = unsafe { *crate::heap::meta(p) };
                    assert!(b & COLOR == self.color && b & YOUNG == 0, "{p:#x} reachable but unmarked ({b:#x})");
                    self.stack.push(p);
                }
            }
        }
        let mut r = Reach {
            seen: Default::default(),
            stack: Vec::new(),
            color,
        };
        unsafe {
            self.each_root(sp, regs, &mut |p| r.cell(p));
            while let Some(p) = r.stack.pop() {
                trace(&mut r, p, 0, usize::MAX);
            }
        }
    }

    /// Call `f` with every cell a root refers to (for `verify_marking`).
    unsafe fn each_root(&mut self, sp: usize, regs: *mut Value, f: &mut dyn FnMut(usize)) {
        struct Each<'a>(&'a mut dyn FnMut(usize));
        impl Visit for Each<'_> {
            unsafe fn cell(&mut self, p: usize) {
                (self.0)(p)
            }
        }
        let range = self.heap.cells_range();
        unsafe {
            let stack = self.stack.as_ptr() as usize;
            let stack_end = self.stack_end as usize;
            let mut top = (regs as usize).max(self.osr_regs as usize).max(self.native_top as usize);
            for a in (sp..self.heap.gc.stack_base).step_by(8) {
                let w = *(a as *const usize);
                if (stack..stack_end).contains(&w) {
                    top = top.max(w);
                } else if let Some(p) = self.heap.cell_containing(w) {
                    f(p);
                }
            }
            let top = (top + 8 * self.max_frame).clamp(stack, stack_end);
            for a in (stack..top).step_by(8) {
                let w = *(a as *const usize);
                if Value::from_bits(w as u64).is_cell() && is_cell_in(range, w) {
                    f(w);
                }
            }
            let mut e = Each(f);
            for v in self.named_roots() {
                e.value(v);
            }
            for i in 0..self.handles.len() {
                e.value(self.handles[i]);
            }
            for i in 0..self.kept_alive.len() {
                e.value(self.kept_alive[i]);
            }
            for i in 0..self.open_upvalues.len() {
                e.payload(self.open_upvalues[i] as usize);
            }
            for s in self.shapes() {
                e.value(s.proto);
            }
            let from = self.heap.gc.codes_from;
            for code in self.codes().filter(|c| c.pinned.get() || c.mark.load(std::sync::atomic::Ordering::Relaxed) >= from) {
                for &c in code.constants.iter() {
                    e.value(c);
                }
                for ways in code.made_ics().iter() {
                    for ic in (*ways.get()).iter() {
                        let h = ic.holder as usize;
                        if self.heap.contains(h) {
                            e.cell(h);
                        }
                    }
                }
            }
            for v in self.string_constants() {
                e.value(v);
            }
            for i in 0..self.heap.gc.kept.len() {
                if let Some(p) = self.heap.cell_containing(self.heap.gc.kept[i]) {
                    e.cell(p);
                }
            }
        }
    }
}

/// What a collection does (see the module docs).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Collection {
    Minor,
    /// A minor collection during a marking cycle.
    MinorShade,
    /// The start pause of a concurrent full collection.
    Start,
    /// Its final pause.
    Final,
    /// A stop-the-world full collection.
    Full,
}

impl Heap {
    /// The color the next full collection marks with (1, 2, 3, 1, …).
    fn next_color(&mut self) -> u8 {
        self.set_color(self.color % 3 + 1);
        self.color
    }

    /// The cell `a` points into, if any.
    pub fn cell_containing(&self, a: usize) -> Option<usize> {
        if !self.contains(a) {
            return None;
        }
        let start = self.cell_start_before(a)?;
        let n = unsafe { cell_size(start as *const u8) };
        (a < start + n).then_some(start)
    }

    /// Mark the young cells the old cells on dirty cards refer to, and
    /// clean the cards (every young cell marked is old afterwards).
    unsafe fn scan_cards(&mut self, m: &mut Marking) {
        unsafe {
            // The dirty cards, cleaned, as runs: each scanned as one range
            // (the cell reaching into it looked for once, and the cells
            // started on one card not traced again from the next), split
            // to at most `CARDS_PER_TAKE` cards for the threads.
            let (base, end) = self.space_range();
            let (first, last) = (crate::heap::card(base) as usize, crate::heap::card(end) as usize);
            let at = |c: usize| base + (c - first) * crate::heap::CARD;
            let mut runs = std::mem::take(&mut self.gc.card_runs);
            runs.clear();
            let mut cards = 0;
            let mut c = first;
            while c < last {
                if c.is_multiple_of(8) && c + 8 <= last && *(c as *const u64) == 0 {
                    c += 8;
                    continue;
                }
                if *(c as *const u8) != 0 {
                    let start = c;
                    while c < last && *(c as *const u8) != 0 && c - start < CARDS_PER_TAKE {
                        *(c as *mut u8) = 0;
                        c += 1;
                    }
                    runs.push((at(start), at(c)));
                    cards += c - start;
                    continue;
                }
                c += 1;
            }
            let threads = self.gc.threads;
            if threads < 2 || cards < PARALLEL_CARDS {
                let mut refs = CardRefs::new(m);
                for &(lo, hi) in &runs {
                    self.scan_card(lo, hi, &mut refs);
                }
                refs.flush();
            } else {
                // On several threads, each taking runs in turn.
                let next = std::sync::atomic::AtomicUsize::new(0);
                let heap = SharedHeap(self);
                let (runs, next) = (&runs[..], &next);
                let (mode, range) = (m.mode, m.heap);
                let work = move || {
                    let heap = heap;
                    let mut cm = CardMarking { mode, heap: range, stack: Vec::new(), shade: Vec::new() };
                    let mut refs = CardRefs::new(&mut cm);
                    loop {
                        let i = next.fetch_add(RUNS_PER_TAKE, std::sync::atomic::Ordering::Relaxed);
                        if i >= runs.len() {
                            break;
                        }
                        for &(lo, hi) in &runs[i..(i + RUNS_PER_TAKE).min(runs.len())] {
                            (*heap.0).scan_card(lo, hi, &mut refs);
                        }
                    }
                    refs.flush();
                    (cm.stack, cm.shade)
                };
                std::thread::scope(|sc| {
                    let helpers: Vec<_> = (1..threads).map(|_| sc.spawn(work)).collect();
                    let mut take = |(stack, shade): (Vec<usize>, Vec<usize>)| {
                        m.stack.extend(stack);
                        m.shade.extend(shade);
                    };
                    take(work());
                    for h in helpers {
                        take(h.join().unwrap());
                    }
                });
            }
            self.gc.card_runs = runs;
        }
    }

    /// Mark the young cells the old cells' words in `lo..hi` refer to,
    /// and all of an object's that starts there: compiled code marks an
    /// object's own card for a store to any of its in-object slots, Rust
    /// code the slot's (see `heap::card_mark`). Old cells are found from
    /// the metadata alone (their first granules have a color), so the
    /// young cells most dirty cards hold, written as they were made, cost
    /// nothing.
    unsafe fn scan_card(&self, lo: usize, hi: usize, m: &mut impl Visit) {
        unsafe {
            // An old cell from before the card reaching into it.
            // (While blocks are swept, a dead old cell keeps an old color:
            // only cells of the current color are live.)
            let live = |b: u8| b & COLOR != 0 && (!self.gc.sweeping || b & COLOR == self.color);
            if let Some(p) = self.cell_start_before(lo)
                && p < lo
                && live(*self.meta(p))
                && p + cell_size(p as *const u8) > lo
            {
                trace(m, p, lo, hi);
            }
            // Old cells starting on it: eight granules' bytes at a time.
            let mut g = lo;
            while g < hi {
                let w = (self.meta(g) as *const u64).read_unaligned();
                if w & 0x0303_0303_0303_0303 == 0 {
                    g += 8 * GRANULE;
                    continue;
                }
                for k in 0..8 {
                    let p = g + k * GRANULE;
                    if live(*self.meta(p)) {
                        if *(p as *const u64) & 1 == 0 {
                            trace(m, p, 0, usize::MAX);
                        } else {
                            trace(m, p, lo, hi);
                        }
                    }
                }
                g += 8 * GRANULE;
            }
        }
    }

    /// Free the young cells a minor collection left unmarked: their
    /// metadata cleared (their memory is holes), and with `poison` filled
    /// with [`POISON`]. Returns the bytes freed (counted only with
    /// `poison`, and large cells).
    ///
    /// Each region's room is counted in `room` (for `collected`): its runs
    /// of free granules allocation would take (at least `min_hole`).
    unsafe fn free_young(&mut self, poison: bool) -> usize {
        let mut freed = 0;
        let regions = std::mem::take(&mut self.regions);
        let min = crate::heap::min_hole() / GRANULE;
        self.room.clear();
        unsafe {
            for &(s, e) in &regions {
                if self.is_large_start(s) {
                    if *self.meta(s) == YOUNG {
                        freed += e - s;
                        self.free_large(s);
                    }
                    self.room.push(0);
                    continue;
                }
                if poison {
                    let mut p = s;
                    while p < e {
                        let n = cell_size(p as *const u8);
                        if *self.meta(p) == YOUNG {
                            *self.meta(p) = 0;
                            std::ptr::write_bytes(p as *mut u8, POISON, n);
                            freed += n;
                        }
                        p += n;
                    }
                    self.room.push(e - s);
                    continue;
                }
                // Clear every `YOUNG` byte (bit 7; colors and `END` are
                // below it), eight at a time, counting the granules in runs
                // of words left zero, two words (`min_hole`) or more: what
                // allocation takes, give or take a hole's ends. (A cell's
                // own zero bytes, between its first and its `END`, count
                // too: only in a cell of more than 256 bytes.)
                let (mut m, me) = (self.meta(s) as usize, self.meta(e) as usize);
                while m < me && !m.is_multiple_of(8) {
                    *(m as *mut u8) &= !YOUNG;
                    m += 1;
                }
                let (mut room, mut run) = (0, 0);
                while m + 8 <= me {
                    let w = *(m as *mut u64) & !0x8080_8080_8080_8080;
                    *(m as *mut u64) = w;
                    if w == 0 {
                        run += 8;
                    } else {
                        if run >= min {
                            room += run;
                        }
                        run = 0;
                    }
                    m += 8;
                }
                if run >= min {
                    room += run;
                }
                while m < me {
                    *(m as *mut u8) &= !YOUNG;
                    m += 1;
                }
                self.room.push(room * GRANULE);
            }
        }
        self.regions = regions;
        freed
    }

    /// Free every cell the full collection that marked with `color` left
    /// unmarked: clear its metadata, and with `poison` fill it with
    /// [`POISON`]. Blocks left with no cell are freed, the others are
    /// allocated in again; every card is clean (no young cell is left).
    /// Returns the bytes freed (counted only with `poison`, and large
    /// cells).
    ///
    /// From the metadata alone, eight granules' bytes at a time where they
    /// are free: a marked or dead old cell has its color at its first
    /// granule and `END` at its last, a young one `YOUNG` at its first (the
    /// headers are read only to poison).
    unsafe fn free_unmarked(&mut self, color: u8, poison: bool) -> usize {
        let mut freed = 0;
        let mut empty = Vec::new();
        let mut partly = Vec::new();
        unsafe {
            for (start, end) in self.block_ranges() {
                if end - start != crate::heap::BLOCK {
                    freed += self.sweep_large(start, color);
                    continue;
                }
                match sweep_block(start, color, poison) {
                    0 => empty.push(start),
                    live => partly.push((start, crate::heap::BLOCK - live)),
                }
            }
            self.clear_cards();
        }
        self.reset_blocks(&partly, &empty);
        freed
    }

    /// Free the large cell at `start` if the marking with `color` left it
    /// unmarked; the bytes freed.
    unsafe fn sweep_large(&mut self, start: usize, color: u8) -> usize {
        unsafe {
            let m = *self.meta(start);
            if m & (YOUNG | COLOR) != 0 && m & COLOR != color {
                let n = cell_size(start as *const u8);
                self.free_large(start);
                return n;
            }
        }
        0
    }

    /// Clean every card: after a full marking, no old cell refers to a
    /// young one.
    unsafe fn clear_cards(&mut self) {
        let (base, end) = self.space_range();
        let (c0, c1) = (crate::heap::card(base), crate::heap::card(end));
        unsafe { std::ptr::write_bytes(c0, 0, c1 as usize - c0 as usize) };
    }

    /// The final pause marked with `color`: free the unmarked large cells,
    /// clean the cards, and hand every small block to the marker thread to
    /// sweep while the mutator runs. Allocation meanwhile takes free and
    /// new blocks, and the swept ones as they come ([`Heap::absorb_sweep`]).
    /// Returns the bytes freed now.
    unsafe fn start_sweep(&mut self, color: u8, poison: bool) -> usize {
        let mut freed = 0;
        let mut small = Vec::new();
        unsafe {
            for (start, end) in self.block_ranges() {
                if end - start != crate::heap::BLOCK {
                    freed += self.sweep_large(start, color);
                } else {
                    small.push(start);
                }
            }
            self.clear_cards();
        }
        // (Blocks swept before are swept again: their cells' colors are
        // stale now.)
        self.reset_blocks(&[], &[]);
        if self.gc.marker.is_none() {
            self.gc.marker = Some(Marker::start());
        }
        let mk = self.gc.marker.as_ref().unwrap();
        let mut st = mk.shared.state.lock().unwrap();
        st.sweep = small;
        st.sweep_color = color;
        st.poison = poison;
        drop(st);
        self.gc.sweeping = true;
        freed
    }

    /// Take what the sweeper found: blocks with room to allocate in, empty
    /// blocks to free. Whether sweeping is over.
    pub(crate) fn absorb_sweep(&mut self) -> bool {
        let Some(mk) = &self.gc.marker else { return true };
        let (partly, empty, over) = {
            let mut st = mk.shared.state.lock().unwrap();
            let over = st.sweep.is_empty() && !st.sweeping;
            (std::mem::take(&mut st.swept), std::mem::take(&mut st.emptied), over)
        };
        self.add_swept(&partly, &empty);
        if over && self.gc.sweeping {
            self.gc.sweeping = false;
            self.gc.full_at = self.allocated() + self.gc.headroom();
        }
        over
    }

    /// Sweep a few blocks on the mutator, rather than take new ones while
    /// the sweeper is behind (JSC's sweep-to-allocate): what it found goes
    /// to allocation. Whether there were any.
    pub(crate) fn help_sweep(&mut self) -> bool {
        let Some(mk) = &self.gc.marker else { return false };
        let (blocks, color, poison) = {
            let mut st = mk.shared.state.lock().unwrap();
            let k = st.sweep.len().saturating_sub(16);
            (st.sweep.split_off(k), st.sweep_color, st.poison)
        };
        if blocks.is_empty() {
            return false;
        }
        let mut partly = Vec::new();
        let mut empty = Vec::new();
        for b in blocks {
            match unsafe { sweep_block(b, color, poison) } {
                0 => empty.push(b),
                live => partly.push((b, crate::heap::BLOCK - live)),
            }
        }
        self.add_swept(&partly, &empty);
        true
    }

    /// Finish sweeping on the mutator (a collection is about to change
    /// the colors).
    pub(crate) fn finish_sweep(&mut self) {
        if !self.gc.sweeping {
            return;
        }
        let Some(mk) = &self.gc.marker else { return };
        mk.pause();
        let (rest, color, poison) = {
            let mut st = mk.shared.state.lock().unwrap();
            (std::mem::take(&mut st.sweep), st.sweep_color, st.poison)
        };
        let mut partly = Vec::new();
        let mut empty = Vec::new();
        for b in rest {
            match unsafe { sweep_block(b, color, poison) } {
                0 => empty.push(b),
                live => partly.push((b, crate::heap::BLOCK - live)),
            }
        }
        self.add_swept(&partly, &empty);
        let over = self.absorb_sweep();
        debug_assert!(over);
    }
}

/// Sweep the small block at `start` after a marking with `color`: clear
/// the metadata of every cell not marked with it, filling it with
/// [`POISON`] if `poison`. Returns the bytes of the cells left.
///
/// From the metadata alone, eight granules' bytes at a time where they
/// are free: a marked or dead old cell has its color at its first granule
/// and `END` at its last, a young one `YOUNG` at its first (headers are
/// read only to poison). The marker thread runs it too: it touches only
/// the block's metadata, which nothing else writes while it is swept.
unsafe fn sweep_block(start: usize, color: u8, poison: bool) -> usize {
    const ENDS: u64 = 0x0404_0404_0404_0404;
    let meta = crate::heap::meta;
    let end = start + crate::heap::BLOCK;
    let mut live = 0;
    let mut p = start;
    unsafe {
        while p < end {
            if p + 8 * GRANULE <= end && (meta(p) as *const u64).read_unaligned() == 0 {
                p += 8 * GRANULE;
                continue;
            }
            let m = *meta(p);
            if m & (YOUNG | COLOR) == 0 {
                p += GRANULE;
                continue;
            }
            if m == YOUNG {
                // Young and unmarked: free (the granules after its first
                // are zero anyway).
                *meta(p) = 0;
                if poison {
                    std::ptr::write_bytes(p as *mut u8, POISON, cell_size(p as *const u8));
                }
                p += GRANULE;
                continue;
            }
            // Old: up to its `END`.
            let mut q = p;
            while q + 8 * GRANULE <= end {
                let w = (meta(q) as *const u64).read_unaligned() & ENDS;
                if w != 0 {
                    q += w.trailing_zeros() as usize / 8 * GRANULE;
                    break;
                }
                q += 8 * GRANULE;
            }
            while *meta(q) & END == 0 {
                q += GRANULE;
            }
            let n = q + GRANULE - p;
            if m & COLOR == color {
                live += n;
            } else {
                *meta(p) = 0;
                *meta(q) = 0;
                if poison {
                    std::ptr::write_bytes(p as *mut u8, POISON, n);
                }
            }
            p = q + GRANULE;
        }
    }
    live
}

/// The first young cell a trace reaches (see `Heap::check_barriers`).
struct Reached<'a> {
    heap: &'a Heap,
    young: Option<usize>,
}

impl Visit for Reached<'_> {
    unsafe fn cell(&mut self, q: usize) {
        let h = self.heap;
        if self.young.is_none() && h.contains(q) && q.is_multiple_of(GRANULE) && unsafe { *h.meta(q) } == YOUNG {
            self.young = Some(q);
        }
    }
}

impl Heap {
    /// Panic on an old cell that refers to a young cell the minor
    /// collection left unmarked: a store without its barrier
    /// (`CLOISON_GC_CHECK_BARRIER`).
    unsafe fn check_barriers(&mut self) {
        unsafe {
            for (start, end) in self.block_ranges() {
                let mut p = start;
                while p < end {
                    let m = *self.meta(p);
                    if m & (YOUNG | COLOR) == 0 {
                        p += GRANULE;
                        continue;
                    }
                    let n = cell_size(p as *const u8);
                    // A dead cell a sweep hasn't reached: what it refers to
                    // may be freed, its room young cells now.
                    if self.gc.sweeping && m & COLOR != 0 && m & COLOR != self.color {
                        p += n;
                        continue;
                    }
                    // What a trace of it follows (not the padding past an
                    // odd number of slots, which no maker writes).
                    if m & COLOR != 0 {
                        let mut r = Reached { heap: self, young: None };
                        trace(&mut r, p, p, p + n);
                        if let Some(q) = r.young {
                            let first = *(p as *const u64);
                            let what = if first & 1 == 0 {
                                format!("{:?}", (*(p as *const Object)).class)
                            } else {
                                format!("kind {}", first & 7)
                            };
                            panic!("unbarriered: old {what} at {p:#x} -> young {q:#x}");
                        }
                    }
                    p += n;
                }
            }
        }
    }
}
