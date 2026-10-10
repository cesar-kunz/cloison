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
//! `TOI_GC_STW` marks full collections in one pause instead.
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
    /// The heap's blocks in use above which the next collection is full.
    full_at: usize,
    /// Every collection full (`TOI_GC_FULL`).
    always_full: bool,
    /// Full collections marked in one pause (`TOI_GC_STW`).
    stw: bool,
    /// Concurrent full collections back to back (`TOI_GC_CYCLES`).
    cycles: bool,
    /// A concurrent full collection is marking.
    marking: bool,
    /// The marker thread is sweeping after a final pause.
    pub(crate) sweeping: bool,
    /// What the heap may grow by before the next full collection (see
    /// [`GROWTH`]).
    room: usize,
    /// The heap's blocks in use when the current cycle started, and what
    /// the last cycle grew them by while marking: the next one starts that
    /// much earlier, so the heap peaks where a stop-the-world collection
    /// would have let it.
    cycle_from: usize,
    lead: usize,
    /// Bytes the last full collection found live (for `TOI_GC_STATS`).
    live: usize,
    /// Bytes minor collections promoted since the last major one.
    promoted: usize,
    /// Old cells a minor collection during marking shaded for the marker.
    shade: Vec<usize>,
    /// The marker thread, once a cycle needed it.
    pub(crate) marker: Option<Marker>,
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
            stw: env("TOI_GC_STW"),
            cycles: env("TOI_GC_CYCLES"),
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

    #[cold]
    #[inline(never)]
    fn report_room(&self) {
        eprintln!("  live {} MiB, room {} MiB, lead {} MiB", self.live >> 20, self.room >> 20, self.lead >> 20);
    }

    /// What the heap may grow by after a sweep before the next cycle
    /// starts: the room less the last cycle's lead, half the room and
    /// [`FULL_MIN`] at least (more cycles of a small heap cost more time
    /// than they save memory: EarleyBoyer 12% at half of 64 MiB).
    fn headroom(&self) -> usize {
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

impl Marking<'_> {
    /// Trace every marked cell until none is left; returns how many there
    /// were. `stop` is asked every few hundred cells whether to stop early
    /// (the marker, asked to pause), leaving the rest on the stack.
    unsafe fn drain(&mut self, stop: &dyn Fn() -> bool) -> usize {
        let mut n = 0;
        // Cells pass through a small ring between the stack and tracing,
        // prefetched as they enter it: by the time one is traced its
        // header has had `RING - 1` other cells' time to arrive.
        const RING: usize = 8;
        let mut ring = [0usize; RING];
        let (mut head, mut len) = (0, 0);
        unsafe {
            loop {
                while len < RING {
                    let Some(p) = self.stack.pop() else { break };
                    prefetch(p);
                    ring[(head + len) % RING] = p;
                    len += 1;
                }
                if len == 0 {
                    break;
                }
                let p = ring[head];
                head = (head + 1) % RING;
                len -= 1;
                let size = cell_size(p as *const u8);
                *crate::heap::meta(p + size - GRANULE) |= END;
                self.marked += size;
                trace(self, p, 0, usize::MAX);
                n += 1;
                if n % 256 == 0 && stop() {
                    break;
                }
            }
            // Stopped early: what the ring holds goes back on the stack.
            for i in 0..len {
                self.stack.push(ring[(head + i) % RING]);
            }
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
            Class::TypedArray => {
                // `elems` points into the buffer's bytes.
                if within(code) {
                    v.value(Value::from_bits((*o).code as u64));
                }
                if within(props) {
                    v.payload((*o).props as usize);
                }
                return;
            }
            Class::Function => {
                let ups = (&(*(*o).code).upvalues).len();
                let up = crate::vm::Vm::upvalues(o);
                for k in 0..ups {
                    if within(up.add(k) as usize) {
                        v.payload(*up.add(k) as usize);
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
            .name("toi-marker".into())
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
        st.run = true;
        st.done = false;
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
        let gc = &self.heap.gc;
        let kind = if gc.marking {
            let done = gc.marker.as_ref().is_some_and(|m| m.shared.state.lock().unwrap().done);
            // The final pause: the marker is done, or the heap has outgrown
            // the cycle (the mutator then finishes the marking).
            if done || self.heap.allocated() > 2 * gc.full_at {
                Collection::Final
            } else if self.heap.since() >= gc.budget() {
                Collection::MinorShade
            } else {
                // (The marker's request of the final pause came early.)
                self.heap.requested.store(false, std::sync::atomic::Ordering::Relaxed);
                return;
            }
        } else if gc.always_full || gc.cycles || self.heap.allocated() > gc.full_at || gc.promoted > gc.room.max(FULL_MIN) {
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
        if let Some(m) = &self.heap.gc.marker {
            m.pause();
        }
        if matches!(kind, Collection::Start | Collection::Full) {
            self.heap.finish_sweep();
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
            marked = m.drain(&|| false);
            if kind == Collection::Minor && self.heap.gc.check_barriers {
                self.heap.check_barriers();
            }
            if kind == Collection::Final && check {
                self.verify_marking(sp, regs, color);
            }
            times[2] = lap();
            freed = match kind {
                Collection::Final => self.heap.start_sweep(color, check),
                Collection::Full => self.heap.free_unmarked(color, check),
                _ => self.heap.free_young(check),
            };
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

    /// Debugging (`TOI_GC_ROOTS`): trace what the roots marked so far
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
            for i in 0..self.open_upvalues.len() {
                m.payload(self.open_upvalues[i] as usize);
            }
            // What Rust structures took since the last collection.
            for i in 0..self.heap.gc.log.len() {
                let a = self.heap.gc.log[i];
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
            for code in self.codes() {
                for &c in code.constants.iter() {
                    m.value(c);
                }
                for ways in code.ics.iter() {
                    for ic in (*ways.get()).iter() {
                        // A load's prototype, a call's callee; a store's
                        // is a field type (`FieldType::encode`), not in
                        // the heap.
                        let h = ic.holder as usize;
                        if self.heap.contains(h) {
                            m.cell(h);
                        }
                    }
                }
            }
            for v in self.string_constants() {
                m.value(v);
            }
            self.roots_report(m, "shapes, code, strings");
        }
    }

    /// Under `TOI_GC_VERIFY`, after the final pause marked: mark again from
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
            for i in 0..self.open_upvalues.len() {
                e.payload(self.open_upvalues[i] as usize);
            }
            for s in self.shapes() {
                e.value(s.proto);
            }
            for code in self.codes() {
                for &c in code.constants.iter() {
                    e.value(c);
                }
                for ways in code.ics.iter() {
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
                    self.scan_card(lo, lo + crate::heap::CARD, m);
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
    unsafe fn scan_card(&mut self, lo: usize, hi: usize, m: &mut Marking) {
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
            if p + 8 * GRANULE <= end && *(meta(p) as *const u64) == 0 {
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
