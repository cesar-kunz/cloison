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
//! Roots are scanned conservatively where their words aren't known to be
//! values: the VM stack (every word checked against the metadata) and the
//! native stack with the registers (interior pointers resolved to their
//! cell).
//!
//! `TOI_GC_VERIFY=<KiB>` collects every that many KiB of allocation (at
//! least a block's), checks every reference it follows, and fills freed
//! cells with [`POISON`], so that a missed root or barrier shows up as a
//! poisoned value on first use; `TOI_GC_STRESS` collects at every
//! safepoint, the same way. `TOI_GC_STATS` prints each collection.

use crate::heap::{BYTES, COLOR, END, GRANULE, Heap, UPVALUE, VALUES, YOUNG, cell_size};
use crate::object::{Class, INLINE_BASE, Object};
use crate::value::Value;
use crate::vm::Vm;

/// The byte freed cells are filled with under `TOI_GC_VERIFY`.
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
    /// what is freed (`TOI_GC_VERIFY`; 0: off).
    pub verify: usize,
    /// Collect at every safepoint (`TOI_GC_STRESS`).
    stress: bool,
    /// Print each collection (`TOI_GC_STATS`).
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
    /// Cells stored into Rust structures since the last collection.
    log: Vec<usize>,
    /// Slots of Rust structures (call caches) a young cell was stored in
    /// since the last collection: what they hold then is a root.
    log_slots: Vec<usize>,
    /// Bytes the current collection marked.
    marked: usize,
    /// The heap's blocks in use above which the next collection is full.
    full_at: usize,
    /// Every collection full (`TOI_GC_FULL`).
    always_full: bool,
    /// Bytes allocated between minor collections (`TOI_GC_NURSERY`, MiB).
    nursery: usize,
    /// Print what each kind of root reaches (`TOI_GC_ROOTS`).
    roots_report: bool,
    /// After each minor collection's marking, look for an old cell that
    /// refers to an unmarked young one, a missed barrier
    /// (`TOI_GC_CHECK_BARRIER`, slow).
    check_barriers: bool,
}

impl Gc {
    pub(crate) fn new() -> Gc {
        let env = |k| std::env::var_os(k).is_some();
        let verify = std::env::var("TOI_GC_VERIFY")
            .ok()
            .map_or(0, |s| s.parse::<usize>().unwrap_or(256).max(1) << 10);
        Gc {
            verify,
            stress: env("TOI_GC_STRESS"),
            stats: env("TOI_GC_STATS"),
            always_full: env("TOI_GC_FULL"),
            check_barriers: env("TOI_GC_CHECK_BARRIER"),
            roots_report: env("TOI_GC_ROOTS"),
            nursery: std::env::var("TOI_GC_NURSERY")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .map_or(NURSERY, |mb| mb << 20),
            full_at: FULL_MIN,
            ..Default::default()
        }
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
        if self.is_young(v) {
            self.gc.log.push(v.to_bits() as usize);
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
            self.gc.log.push(a);
        }
    }
}

/// How a collection marks.
#[derive(Clone, Copy)]
struct Mode {
    color: u8,
    /// Marks young cells only.
    minor: bool,
    /// Checks every reference it follows.
    check: bool,
}

/// Mark the cell at `p` and push it, unless it is marked (or, in a minor
/// collection, old).
///
/// # Safety
/// `p` must be a cell.
#[inline(always)]
unsafe fn mark(heap: &mut Heap, p: usize, mode: Mode) {
    unsafe {
        let m = heap.meta(p);
        if if mode.minor { *m != YOUNG } else { *m & COLOR == mode.color } {
            return;
        }
        let n = cell_size(p as *const u8);
        *m = mode.color;
        *heap.meta(p + n - GRANULE) |= END;
        heap.gc.marked += n;
        heap.gc.stack.push(p);
    }
}

/// Whether there is a cell at `p`, by the metadata: allocated since the
/// last collection or marked by one.
#[inline(always)]
fn is_cell_at(heap: &Heap, p: usize) -> bool {
    heap.contains(p) && p.is_multiple_of(GRANULE) && unsafe { *heap.meta(p) } & (YOUNG | COLOR) != 0
}

/// Mark `v` if it is a cell.
#[inline(always)]
unsafe fn mark_value(heap: &mut Heap, v: Value, mode: Mode) {
    if !v.is_cell() {
        return;
    }
    let p = v.to_bits() as usize;
    if mode.check && !is_cell_at(heap, p) {
        bad_reference(heap, p);
    }
    unsafe { mark(heap, p, mode) }
}

#[cold]
#[inline(never)]
fn bad_reference(heap: &Heap, p: usize) -> ! {
    let m = if heap.contains(p) { unsafe { *heap.meta(p & !(GRANULE - 1)) } } else { 0 };
    panic!("reference to {p:#x}, which is no cell (metadata {m:#x})");
}

/// Mark the cell an array's or upvalue's payload pointer `q` points into
/// (past its header word), if not null.
#[inline(always)]
unsafe fn mark_payload(heap: &mut Heap, q: usize, mode: Mode) {
    if q == 0 {
        return;
    }
    let p = q - 8;
    if mode.check && !is_cell_at(heap, p) {
        bad_reference(heap, p);
    }
    unsafe { mark(heap, p, mode) }
}

/// Mark what the cell at `p` refers to from its words in `lo..hi` (see
/// "Cells" in `docs/gc.md`).
///
/// # Safety
/// `p` must be a cell.
unsafe fn trace(heap: &mut Heap, p: usize, mode: Mode, lo: usize, hi: usize) {
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
                        mark_value(heap, *(a as *const Value), mode);
                        a += 8;
                    }
                }
                UPVALUE => {
                    if within(p + 16) {
                        mark_value(heap, *((p + 16) as *const Value), mode);
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
            mark_value(heap, *(a as *const Value), mode);
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
                        mark_value(heap, Value::from_bits((*o).props as u64), mode);
                    }
                    if within(code) {
                        mark_value(heap, Value::from_bits((*o).code as u64), mode);
                    }
                } else if (*o).elems as usize != p + INLINE_BASE && within(elems) {
                    // Flattened: its units' buffer.
                    mark_payload(heap, (*o).elems as usize, mode);
                }
                return;
            }
            Class::TypedArray => {
                // `elems` points into the buffer's bytes.
                if within(code) {
                    mark_value(heap, Value::from_bits((*o).code as u64), mode);
                }
                if within(props) {
                    mark_payload(heap, (*o).props as usize, mode);
                }
                return;
            }
            Class::Function => {
                let ups = (&(*(*o).code).upvalues).len();
                let up = crate::vm::Vm::upvalues(o);
                for k in 0..ups {
                    if within(up.add(k) as usize) {
                        mark_payload(heap, *up.add(k) as usize, mode);
                    }
                }
            }
            _ => {}
        }
        if within(props) {
            mark_payload(heap, (*o).props as usize, mode);
        }
        if within(elems) {
            mark_payload(heap, (*o).elems as usize, mode);
        }
    }
}

/// Mark the cell that word `w` points to the start of, if any (a word of
/// the VM stack, a pending argument: values, or stale ones).
#[inline(always)]
unsafe fn mark_exact(heap: &mut Heap, w: usize, mode: Mode) {
    if Value::from_bits(w as u64).is_cell() && is_cell_at(heap, w) {
        unsafe { mark(heap, w, mode) }
    }
}

/// Mark the cell that word `w` points into, if any (a native stack word,
/// a Rust structure's address of a slot).
unsafe fn mark_interior(heap: &mut Heap, w: usize, mode: Mode) {
    unsafe {
        if let Some(p) = heap.cell_containing(w) {
            mark(heap, p, mode);
        }
    }
}

/// Run `f` with the callee-saved registers spilled into this frame, and
/// the stack pointer below them: every word a caller holds is then in
/// memory at or above it.
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

impl Vm {
    /// A safepoint: collect if the heap asks for it. `regs` is the running
    /// frame's registers (or null).
    #[inline(always)]
    pub(crate) fn safepoint(&mut self, regs: *mut Value) {
        if self.heap.requested && self.heap.gc.stack_base != 0 {
            self.collect(regs);
        }
    }

    #[cold]
    #[inline(never)]
    fn collect(&mut self, regs: *mut Value) {
        let vm: *mut Vm = self;
        let full = self.heap.gc.always_full || self.heap.allocated() > self.heap.gc.full_at;
        with_registers_spilled(&mut |sp| unsafe { (*vm).collect_from(sp, regs, full) });
    }

    /// A collection, the native stack's live part starting at `sp`.
    fn collect_from(&mut self, sp: usize, regs: *mut Value, full: bool) {
        let start = std::time::Instant::now();
        let check = self.heap.gc.checking();
        let color = if full { self.heap.next_color() } else { self.heap.color };
        let mode = Mode {
            color,
            minor: !full,
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
        self.heap.gc.marked = 0;
        unsafe {
            self.mark_roots(sp, regs, mode);
            times[0] = lap();
            if full {
                self.mark_structures(mode);
            } else {
                self.heap.scan_cards(mode);
            }
            times[1] = lap();
            let mut n = 0;
            while let Some(p) = self.heap.gc.stack.pop() {
                trace(&mut self.heap, p, mode, 0, usize::MAX);
                n += 1;
            }
            marked = n;
            if !full && self.heap.gc.check_barriers {
                self.heap.check_barriers();
            }
            times[2] = lap();
            freed = if full {
                self.heap.free_unmarked(color, check)
            } else {
                self.heap.free_young(check)
            };
            times[3] = lap();
        }
        self.heap.gc.log.clear();
        self.heap.gc.log_slots.clear();
        self.heap.gc.count += 1;
        if self.heap.gc.stats {
            let (top, recycle, free, large) = self.heap.block_counts();
            eprintln!(
                "gc {} {}: {:.3} ms (roots {:.3}, {} {:.3}, trace {:.3}, free {:.3}), {marked} cells marked, \
                 {} KiB freed, {} MiB of blocks ({top} blocks, {recycle} to reuse, {free} free, {large} large)",
                self.heap.gc.count,
                if full { "full" } else { "minor" },
                start.elapsed().as_secs_f64() * 1e3,
                times[0],
                if full { "structures" } else { "cards" },
                times[1],
                times[2],
                times[3],
                freed >> 10,
                self.heap.allocated() >> 20
            );
        }
        if full {
            // The blocks the sweep left in use (partly free ones too),
            // and room to grow by what was live, times `GROWTH - 1`.
            let room = ((GROWTH - 1) * self.heap.gc.marked).max(FULL_MIN);
            self.heap.gc.full_at = self.heap.allocated() + room;
        }
        let next = self.heap.gc.budget();
        self.heap.collected(next, full);
        if self.heap.gc.stress {
            self.heap.requested = true;
        }
    }

    /// Debugging (`TOI_GC_ROOTS`): trace what the roots marked so far
    /// reach, and print how many cells that was, for root kind `what`.
    unsafe fn roots_report(&mut self, what: &str, mode: Mode) {
        if !self.heap.gc.roots_report {
            return;
        }
        let mut n = 0;
        unsafe {
            while let Some(p) = self.heap.gc.stack.pop() {
                trace(&mut self.heap, p, mode, 0, usize::MAX);
                n += 1;
            }
        }
        eprintln!("  roots {what}: {n} cells");
    }

    /// Mark the roots every collection has.
    unsafe fn mark_roots(&mut self, sp: usize, regs: *mut Value, mode: Mode) {
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
                    mark_interior(&mut self.heap, w, mode);
                }
            }
            self.roots_report("native stack", mode);
            // The VM stack, to the end of the deepest frame there may be.
            let top = (top + 8 * self.max_frame).clamp(stack, stack_end);
            for a in (stack..top).step_by(8) {
                mark_exact(&mut self.heap, *(a as *const usize), mode);
            }
            self.roots_report(&format!("vm stack ({} words)", (top - stack) / 8), mode);
            let (args, n) = self.pending_args;
            if !args.is_null() {
                for i in 0..n {
                    mark_exact(&mut self.heap, (*args.add(i)).to_bits() as usize, mode);
                }
            }
            // The VM's own references.
            for v in self.named_roots() {
                mark_value(&mut self.heap, v, mode);
            }
            self.roots_report("named", mode);
            for i in 0..self.handles.len() {
                let v = self.handles[i];
                mark_value(&mut self.heap, v, mode);
            }
            for i in 0..self.open_upvalues.len() {
                let u = self.open_upvalues[i] as usize;
                mark_payload(&mut self.heap, u, mode);
            }
            // What Rust structures took since the last collection.
            for i in 0..self.heap.gc.log.len() {
                let a = self.heap.gc.log[i];
                mark_interior(&mut self.heap, a, mode);
            }
            for i in 0..self.heap.gc.log_slots.len() {
                let h = *(self.heap.gc.log_slots[i] as *const usize);
                mark_value(&mut self.heap, Value::from_bits(h as u64), mode);
            }
            self.roots_report("handles, upvalues, log", mode);
        }
    }

    /// Mark what Rust structures refer to: shapes, code, compiled code, the
    /// VM's tables.
    unsafe fn mark_structures(&mut self, mode: Mode) {
        unsafe {
            let heap: *mut Heap = &mut self.heap;
            for i in 0..self.heap.gc.kept.len() {
                let a = self.heap.gc.kept[i];
                mark_interior(&mut self.heap, a, mode);
            }
            self.roots_report("kept", mode);
            for s in self.shapes() {
                mark_value(&mut *heap, s.proto, mode);
            }
            for code in self.codes() {
                for &c in code.constants.iter() {
                    mark_value(&mut *heap, c, mode);
                }
                for ways in code.ics.iter() {
                    for ic in (*ways.get()).iter() {
                        // A load's prototype, a call's callee; a store's
                        // is a field type (`FieldType::encode`), not in
                        // the heap.
                        let h = ic.holder as usize;
                        if (*heap).contains(h) {
                            if mode.check && !is_cell_at(&*heap, h) {
                                bad_reference(&*heap, h);
                            }
                            mark(&mut *heap, h, mode);
                        }
                    }
                }
            }
            for v in self.string_constants() {
                mark_value(&mut *heap, v, mode);
            }
            self.roots_report("shapes, code, strings", mode);
        }
    }
}

impl Heap {
    /// The color the next full collection marks with (1, 2, 3, 1, …).
    fn next_color(&mut self) -> u8 {
        self.color = self.color % 3 + 1;
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
    unsafe fn scan_cards(&mut self, mode: Mode) {
        unsafe {
            let (base, end) = self.space_range();
            let (first, last) = (crate::heap::card(base) as usize, crate::heap::card(end) as usize);
            let mut c = first;
            while c < last {
                if c.is_multiple_of(8) && c + 8 <= last && *(c as *const u64) == 0 {
                    c += 8;
                    continue;
                }
                if *(c as *const u8) != 0 {
                    *(c as *mut u8) = 0;
                    let lo = base + (c - first) * crate::heap::CARD;
                    self.scan_card(lo, lo + crate::heap::CARD, mode);
                }
                c += 1;
            }
        }
    }

    /// Mark the young cells the old cells' words in `lo..hi` refer to,
    /// and all of an object's that starts there: compiled code marks an
    /// object's own card for a store to any of its in-object slots, Rust
    /// code the slot's (see `heap::card_mark`). Old cells are found from
    /// the metadata alone (their first granules have a color), so the
    /// young cells most dirty cards hold, written as they were made, cost
    /// nothing.
    unsafe fn scan_card(&mut self, lo: usize, hi: usize, mode: Mode) {
        unsafe {
            // An old cell from before the card reaching into it.
            if let Some(p) = self.cell_start_before(lo)
                && p < lo
                && *self.meta(p) & COLOR != 0
                && p + cell_size(p as *const u8) > lo
            {
                trace(self, p, mode, lo, hi);
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
                    if *self.meta(p) & COLOR != 0 {
                        if *(p as *const u64) & 1 == 0 {
                            trace(self, p, mode, 0, usize::MAX);
                        } else {
                            trace(self, p, mode, lo, hi);
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
    unsafe fn free_young(&mut self, poison: bool) -> usize {
        let mut freed = 0;
        let regions = std::mem::take(&mut self.regions);
        unsafe {
            for &(s, e) in &regions {
                if self.is_large_start(s) {
                    if *self.meta(s) == YOUNG {
                        freed += e - s;
                        self.free_large(s);
                    }
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
                    continue;
                }
                // Clear every `YOUNG` byte (bit 7; colors and `END` are
                // below it), eight at a time.
                let (mut m, me) = (self.meta(s) as usize, self.meta(e) as usize);
                while m < me && !m.is_multiple_of(8) {
                    *(m as *mut u8) &= !YOUNG;
                    m += 1;
                }
                while m + 8 <= me {
                    *(m as *mut u64) &= !0x8080_8080_8080_8080;
                    m += 8;
                }
                while m < me {
                    *(m as *mut u8) &= !YOUNG;
                    m += 1;
                }
            }
        }
        self.regions = regions;
        freed
    }

    /// Free every cell the full collection that marked with `color` left
    /// unmarked: clear its metadata, and with `poison` fill it with
    /// [`POISON`]. Blocks left with no cell are freed, the others are
    /// allocated in again; every card is clean (no young cell is left).
    /// Returns the bytes freed.
    unsafe fn free_unmarked(&mut self, color: u8, poison: bool) -> usize {
        let mut freed = 0;
        let mut empty = Vec::new();
        let mut partly = Vec::new();
        unsafe {
            for (start, end) in self.block_ranges() {
                let mut live = false;
                let mut p = start;
                while p < end {
                    let m = *self.meta(p);
                    if m & (YOUNG | COLOR) == 0 {
                        p += GRANULE;
                        continue;
                    }
                    let n = cell_size(p as *const u8);
                    if m & COLOR != color {
                        freed += n;
                        if self.is_large_start(p) {
                            self.free_large(p);
                            break;
                        }
                        *self.meta(p) = 0;
                        *self.meta(p + n - GRANULE) = 0;
                        if poison {
                            std::ptr::write_bytes(p as *mut u8, POISON, n);
                        }
                    } else {
                        live = true;
                    }
                    p += n;
                }
                if end - start == crate::heap::BLOCK {
                    if live { partly.push(start) } else { empty.push(start) }
                }
            }
            let (base, end) = self.space_range();
            let (c0, c1) = (crate::heap::card(base), crate::heap::card(end));
            std::ptr::write_bytes(c0, 0, c1 as usize - c0 as usize);
        }
        self.reset_blocks(&partly, &empty);
        freed
    }
}

impl Heap {
    /// Panic on an old cell that refers to a young cell the minor
    /// collection left unmarked: a store without its barrier
    /// (`TOI_GC_CHECK_BARRIER`).
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
                    if m & COLOR != 0 {
                        for a in (p..p + n).step_by(8) {
                            let w = *(a as *const usize);
                            for q in [w, w.wrapping_sub(8)] {
                                if self.contains(q) && q.is_multiple_of(GRANULE) && *self.meta(q) == YOUNG {
                                    let first = *(p as *const u64);
                                    let what = if first & 1 == 0 {
                                        format!("{:?}", (*(p as *const Object)).class)
                                    } else {
                                        format!("kind {}", first & 7)
                                    };
                                    panic!(
                                        "unbarriered: old {what} at {p:#x} (+{}) -> young {q:#x}",
                                        a - p
                                    );
                                }
                            }
                        }
                    }
                    p += n;
                }
            }
        }
    }
}
