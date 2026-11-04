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

use crate::heap::{BYTES, COLOR, END, EVAC, GRANULE, Heap, UPVALUE, VALUES, WEAK_VALUES, YOUNG, cell_size};
use crate::object::{Class, Object};
use crate::value::Value;
use crate::vm::Vm;

/// The byte freed cells are filled with under `CLOISON_GC_VERIFY`.
pub const POISON: u8 = 0xdd;
/// Bytes allocated between minor collections: from `NURSERY_MIN`, doubled
/// up to `NURSERY` while minor collections take more than
/// 1/`NURSERY_OVERHEAD` of the time. A program that allocates little or
/// whose young cells all die keeps a small nursery (peak RSS at 32 MiB and
/// now: Richards 40 MB and 15, Crypto 46 and 22, RegExp 79 and 56, 3%
/// faster); one whose minor collections cost grows it, fewer of them
/// promoting less (fixed at 4 MiB RayTrace lost 35%, the garbage it
/// promoted, not collected below `FULL_MIN`, leaving allocation small
/// holes; EarleyBoyer 27%, its pauses). From 4 MiB Box2D lost 1.7%.
const NURSERY: usize = 32 << 20;
const NURSERY_MIN: usize = 8 << 20;
const NURSERY_OVERHEAD: u64 = 50;
/// Minor collections the overhead is measured over before a resize.
const NURSERY_SAMPLE: u32 = 4;
/// A full collection runs when the heap's blocks have grown, since the
/// last one, by this many times what it left marked less one, and at least
/// [`FULL_MIN`].
const GROWTH: usize = 2;
/// A full marking collects shapes (`Vm::sweep_shapes`) once this many have
/// been made since the last that did, and an eighth of the shapes there
/// are: marking them costs every object traced a check (Splay, which makes
/// few, spent 1.5% of its cycles on it in every marking).
const SHAPE_SWEEP_MIN: usize = 256;
/// A full collection also runs once the shapes made since the last that
/// collected them are as many as the shapes it left, and at least this
/// many (about 48 MB): shapes are outside the heap, so a program whose
/// garbage is mostly shapes (objects' unique ones, prototypes') would
/// otherwise rarely start one. (From 65,536, sass's 4.4M shapes cost it
/// 2.5% more instructions, from 262,144 0.5%, for a peak a little lower.)
const SHAPE_FULL_MIN: usize = 1 << 18;
/// And once the field cells made since are as many as those it left, and
/// at least this many (with their slot tables', about 32 MB): a `delete`
/// gives an object a unique shape with a cell for each field left (one
/// of 32 properties made 32 cells, 1.2 KB, a delete).
const CELL_FULL_MIN: usize = 1 << 20;
const FULL_MIN: usize = 64 << 20;

/// The collector's state, in the heap.
/// What a collection's shape phases took (`CLOISON_GC_STATS`), in
/// milliseconds, and what they freed: the condemned shapes dropped and the
/// field cells left unused freed (`Vm::free_condemned_shapes`), the live
/// shapes found (`mark_live_codes_and_shapes`), the constructor tracking
/// and shapes not found live swept (`Vm::sweep_shapes`) and the zombies
/// nothing holds condemned (`Vm::free_zombies`).
#[derive(Default)]
pub(crate) struct ShapePhases {
    pub(crate) drop: f64,
    pub(crate) dropped: usize,
    pub(crate) cells: f64,
    pub(crate) cells_freed: usize,
    pub(crate) cells_left: usize,
    pub(crate) mark: f64,
    pub(crate) ctor: f64,
    pub(crate) sweep: f64,
    pub(crate) dead: usize,
    pub(crate) shapes_left: usize,
    pub(crate) zombies: f64,
    pub(crate) condemned: usize,
    pub(crate) zombies_left: usize,
    pub(crate) any: bool,
}

impl ShapePhases {
    #[cold]
    #[inline(never)]
    fn report(&self) {
        eprintln!(
            "  shapes: drop {:.3} ({} shapes), cells {:.3} ({} freed, {} left), mark {:.3}, ctor {:.3}, \
             sweep {:.3} ({} dead, {} left), zombies {:.3} ({} condemned, {} left)",
            self.drop,
            self.dropped,
            self.cells,
            self.cells_freed,
            self.cells_left,
            self.mark,
            self.ctor,
            self.sweep,
            self.dead,
            self.shapes_left,
            self.zombies,
            self.condemned,
            self.zombies_left
        );
    }
}

/// Milliseconds since `t`, and `t` now.
pub(crate) fn lap(t: &mut std::time::Instant) -> f64 {
    let now = std::time::Instant::now();
    let d = (now - *t).as_secs_f64() * 1e3;
    *t = now;
    d
}

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
    pub(crate) stats: bool,
    /// What this collection's shape phases took, for `stats`.
    pub(crate) shape_phases: ShapePhases,
    /// Collections so far.
    pub count: usize,
    /// The native stack's base: the highest address roots may be at
    /// (`Vm::run` records it).
    pub stack_base: usize,
    /// Cells Rust structures refer to (compiled code's constants, call
    /// sites' closures), each an address in or at a cell, with how many
    /// live owners refer to it (see [`KeptCells`]).
    kept: KeptTable,
    /// Addresses taken by [`Heap::keep`] for an owner not made yet (a
    /// lowering, a compilation): roots until [`Heap::kept_since`] hands
    /// them to it.
    pending_kept: Vec<usize>,
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
    pub(crate) marking: bool,
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
    /// The full marking under way, or the last, collects shapes (see
    /// [`SHAPE_SWEEP_MIN`]).
    pub(crate) shape_cycle: bool,
    /// Bytes the start pause of the current marking cycle marked with its
    /// color (the young cells it promoted): live when the cycle began, as a
    /// stop-the-world collection would have found them, but not the final
    /// pause's or the marker's to count. (Not the cells later minor
    /// collections promote: made since, they are marked as they come, dead
    /// or not: Splay's live peak read 241 MiB with them, 140 stop-the-world.)
    start_marked: usize,
    /// Old cells a minor collection during marking shaded for the marker.
    shade: Vec<usize>,
    /// The marker thread, once a cycle needed it.
    pub(crate) marker: Option<Marker>,
    /// Bytes allocated between minor collections (`CLOISON_GC_NURSERY`, MiB:
    /// fixed), and whether it grows (see [`NURSERY`]).
    nursery: usize,
    nursery_fixed: bool,
    /// Since the last resize: minor collections, their pauses, and the time
    /// from the end of the collection before the first to the end of the
    /// last (ns).
    minors_sampled: u32,
    minor_pause_ns: u64,
    minor_span_ns: u64,
    /// When the last collection ended.
    last_end: Option<std::time::Instant>,
    /// Print what each kind of root reaches (`CLOISON_GC_ROOTS`).
    roots_report: bool,
    /// After each stop-the-world full collection, print the small blocks'
    /// occupancy and what in them could not move (`CLOISON_GC_FRAG`).
    frag_report: bool,
    /// After each full collection stopping the world, that every cell it
    /// marked is reached from the roots' slots (`CLOISON_GC_SLOTS`, see
    /// `Vm::check_root_slots`).
    slots_check: bool,
    /// What full collections move ([`Vm::evacuate`]).
    evacuate: Evacuate,
    /// The stress mode's blocks this time: odd or even ones.
    evacuate_parity: bool,
    /// The slots into the cycle's evacuation candidates its start pause
    /// and minor collections traced (the marker's are its own).
    evac_rec: Vec<usize>,
    /// Cycles to start without candidates, and how many the next
    /// fruitless evacuation makes it (see `Vm::evacuate_from`).
    evac_wait: u32,
    evac_backoff: u32,
    /// When the last cycle started, when the last evacuation ended and how
    /// long it took (see `Policy::interval`, `Policy::spacing`).
    cycle_start: Option<std::time::Instant>,
    evac_end: Option<std::time::Instant>,
    evac_took: std::time::Duration,

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
            frag_report: env("CLOISON_GC_FRAG"),
            slots_check: env("CLOISON_GC_SLOTS"),
            evacuate: match std::env::var("CLOISON_GC_EVACUATE").as_deref() {
                Ok("0" | "off") => Evacuate::Off,
                Ok("1" | "all") => Evacuate::All,
                _ => Evacuate::Fragmented,
            },
            nursery: std::env::var("CLOISON_GC_NURSERY")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .map_or(NURSERY_MIN, |mb| mb << 20),
            nursery_fixed: env("CLOISON_GC_NURSERY"),
            full_at: FULL_MIN,
            threads: std::env::var("CLOISON_GC_THREADS")
                .ok()
                .and_then(|s| s.parse::<usize>().ok())
                .unwrap_or_else(|| std::thread::available_parallelism().map_or(1, |n| n.get().min(8)))
                .max(1),
            ..Default::default()
        }
    }

    /// See `Vm::set_gc_evacuate_all`.
    pub(crate) fn set_evacuate_all(&mut self) {
        self.always_full = true;
        self.evacuate = Evacuate::All;
    }

    /// An evacuation not worth it (`why`): the next cycles choose no
    /// candidates, twice as many each time, up to 16.
    fn fruitless(&mut self, why: &str) {
        self.evac_backoff = (2 * self.evac_backoff).clamp(1, 16);
        self.evac_wait = self.evac_backoff;
        if self.stats {
            eprintln!("  evacuation skipped: {why}, next {} cycles choose none", self.evac_wait);
        }
    }

    /// See `Vm::set_gc_evacuate_cycles`.
    pub(crate) fn set_evacuate_cycles(&mut self) {
        self.cycles = true;
        self.evacuate = Evacuate::All;
        self.slots_check = true;
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

    /// A minor collection that took `pause` ended at `end`: double the
    /// nursery if minor collections took too much of the time since the
    /// last resize.
    fn sample_minor(&mut self, pause: u64, end: std::time::Instant) {
        if let Some(last) = self.last_end {
            self.minors_sampled += 1;
            self.minor_pause_ns += pause;
            self.minor_span_ns += (end - last).as_nanos() as u64;
        }
        if self.minors_sampled >= NURSERY_SAMPLE {
            if !self.nursery_fixed
                && self.nursery < NURSERY
                && self.minor_pause_ns * NURSERY_OVERHEAD > self.minor_span_ns
            {
                self.nursery *= 2;
            }
            self.minors_sampled = 0;
            self.minor_pause_ns = 0;
            self.minor_span_ns = 0;
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
        let s = &mut self.gc.log_slots;
        // Full: without its repeats before it grows. (A call site turning
        // between young callees logs its ways at every turn: interpreted,
        // Richards logged the same few slots 34 million times before its
        // first minor collection, 272 MB, and that collection read them in
        // 35 ms.)
        if s.len() == s.capacity() && s.len() >= 1024 {
            s.sort_unstable();
            s.dedup();
        }
        s.push(slot as usize);
    }

    /// Forget the slots logged at addresses `from..to` (see `note_slot`):
    /// the structure holding them freed.
    pub(crate) fn forget_slots(&mut self, from: usize, to: usize) {
        self.gc.log_slots.retain(|s| !(from..to).contains(s));
    }

    /// A Rust structure about to be made refers to the cell at or around
    /// address `a`: compiled code embedding it, a call site caching it. A
    /// root until [`Heap::kept_since`] gives it to that structure.
    pub fn keep(&mut self, a: usize) {
        // (Only an address in a cell: what isn't one marks nothing, and
        // needn't be listed.)
        if self.contains(a) && self.gc.pending_kept.last() != Some(&a) && self.cell_containing(a).is_some() {
            self.gc.pending_kept.push(a);
            self.gc.log_kept.push(a);
        }
    }

    /// The addresses Rust structures refer to (see [`KeptCells`]), and
    /// those taken for structures not made yet.
    pub(crate) fn kept_addrs(&self) -> Vec<usize> {
        let t = self.gc.kept.borrow();
        let mut v = Vec::with_capacity(t.len() + self.gc.pending_kept.len());
        v.extend(t.keys().copied());
        v.extend_from_slice(&self.gc.pending_kept);
        v
    }

    /// Where the addresses kept from now on start (see `kept_since`).
    pub(crate) fn kept_mark(&self) -> usize {
        self.gc.pending_kept.len()
    }

    /// The addresses kept since mark `from`, for a structure that refers to
    /// them while it lives.
    pub(crate) fn kept_since(&mut self, from: usize) -> KeptCells {
        let from = from.min(self.gc.pending_kept.len());
        let mut addrs = self.gc.pending_kept.split_off(from);
        addrs.sort_unstable();
        addrs.dedup();
        // (`split_off(0)` hands over the whole vector, its capacity too,
        // and leaves one as large: every owner's list took the most
        // `pending_kept` ever held, the TypeScript app's 24 MiB of them.)
        addrs.shrink_to_fit();
        let mut k = KeptCells { addrs: Vec::new(), table: Some(self.gc.kept.clone()) };
        {
            let mut t = self.gc.kept.borrow_mut();
            for &a in &addrs {
                *t.entry(a).or_default() += 1;
            }
        }
        k.addrs = addrs;
        k
    }

    /// Add address `a` to what `k` keeps, at once.
    pub(crate) fn keep_in(&mut self, k: &mut KeptCells, a: usize) {
        if k.addrs.contains(&a) || self.cell_containing(a).is_none() {
            return;
        }
        if k.table.is_none() {
            k.table = Some(self.gc.kept.clone());
        }
        *self.gc.kept.borrow_mut().entry(a).or_default() += 1;
        k.addrs.push(a);
        self.gc.log_kept.push(a);
    }
}

/// The cells Rust structures refer to by address, each with how many live
/// owners (a [`KeptCells`]) refer to it: roots at every full collection.
pub(crate) type KeptTable = std::rc::Rc<std::cell::RefCell<crate::jit::HashMap<usize, u32>>>;

/// The cells a structure refers to by address (compiled code's constants,
/// a lowering's call sites' closures): roots while it lives.
#[derive(Default)]
pub(crate) struct KeptCells {
    addrs: Vec<usize>,
    table: Option<KeptTable>,
}

impl Drop for KeptCells {
    fn drop(&mut self) {
        if let Some(t) = &self.table {
            let mut t = t.borrow_mut();
            for a in &self.addrs {
                if let std::collections::hash_map::Entry::Occupied(mut e) = t.entry(*a) {
                    *e.get_mut() -= 1;
                    if *e.get() == 0 {
                        e.remove();
                    }
                }
            }
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
    /// Marks the shapes of the objects it traces (see
    /// [`mark_object_shape`]): a full marking that collects shapes
    /// (`Gc::shape_cycle`), and its start pause, which promotes young cells
    /// made before it. Not the minor collections during the marking: the
    /// cells they promote were made since it began, with shapes made since,
    /// pinned, the VM's, or given out by `Vm::root_shape`, `Vm::ctor_root`
    /// or `Vm::create_object`, all live.
    shapes: bool,
}

/// A marking's visitor, recording the slots into evacuation candidates it
/// traces (see [`Candidates::note`]): only the loops of a marking with
/// candidates (a full one stopping the world, or a cycle's pauses, cards
/// and marker) trace through it, so the others' are as before (a check at
/// every slot cost Splay 3% of its instructions).
trait Records: Visit {
    fn cand(&self) -> Candidates;
    fn rec(&mut self) -> &mut Vec<usize>;
}

struct Rec<'x, V: Records>(&'x mut V);

impl<V: Records> Visit for Rec<'_, V> {
    #[inline(always)]
    unsafe fn cell(&mut self, p: usize) {
        unsafe { self.0.cell(p) }
    }

    #[inline(always)]
    unsafe fn slot(&mut self, a: *mut Value) {
        unsafe {
            let v = *a;
            if v.is_cell() {
                let c = self.0.cand();
                c.note(self.0.rec(), a as usize, v.to_bits() as usize, false);
            }
            self.0.value(v)
        }
    }

    #[inline(always)]
    unsafe fn payload_slot(&mut self, a: *mut usize) {
        unsafe {
            let q = *a;
            if q != 0 {
                let c = self.0.cand();
                c.note(self.0.rec(), a as usize, q - 8, true);
            }
            self.0.payload(q)
        }
    }
}

impl Records for Marking<'_> {
    fn cand(&self) -> Candidates {
        self.cand
    }
    fn rec(&mut self) -> &mut Vec<usize> {
        &mut self.rec
    }
}

impl<V: Records> Records for CardRefs<'_, V> {
    fn cand(&self) -> Candidates {
        self.m.cand()
    }
    fn rec(&mut self) -> &mut Vec<usize> {
        self.m.rec()
    }
}

impl Records for CardMarking {
    fn cand(&self) -> Candidates {
        self.cand
    }
    fn rec(&mut self) -> &mut Vec<usize> {
        &mut self.rec
    }
}

/// The evacuation candidates a marking records slots into: a block's
/// byte nonzero in `map` (an address, 0 for none: shared by the threads
/// tracing, so plain integers), from `base`.
#[derive(Clone, Copy, Default)]
struct Candidates {
    map: usize,
    len: usize,
    base: usize,
}

impl Candidates {
    /// Whether `a` (any address) is in a candidate block.
    #[inline(always)]
    fn has(&self, a: usize) -> bool {
        let b = a.wrapping_sub(self.base) / crate::heap::BLOCK;
        b < self.len && unsafe { *((self.map + b) as *const u8) } != 0
    }

    /// Record slot `a`, of a value referring to `to` (or, `payload` set, of
    /// a payload pointer: bit 0 then set), if `to` is in a candidate block
    /// and `a` isn't: a candidate's own cells are traced again when
    /// evacuated.
    #[inline(always)]
    fn note(&self, rec: &mut Vec<usize>, a: usize, to: usize, payload: bool) {
        if self.has(to) && !self.has(a) {
            rec.push(a | payload as usize);
        }
    }
}

/// Full markings so far, over every VM: a code found live is marked with
/// the epoch then (see `vm::Code::mark`).
static CODE_EPOCH: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(1);

/// Totals over every VM's collections (`cloison` prints them with
/// `CLOISON_SIZES` set, `scripts/octane-memory.sh` reads them): pauses,
/// minor and full (a concurrent full collection's two pauses count as one
/// full), their nanoseconds and the longest; the bytes of blocks in use at
/// the start of a collection (where the heap peaks), and the most a full
/// collection found live; the bytes allocated before a collection (not
/// those after the last), and those minor collections (and a cycle's start
/// pause) found live, old from then on.
pub static MINORS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static FULLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PAUSE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static MAX_PAUSE_NS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static HEAP_PEAK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static LIVE_PEAK: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static ALLOCATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static PROMOTED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Bytes evacuation moved (`Vm::evacuate`).
pub static EVACUATED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
/// Full collections that came while no cell could move (`Vm::unmovable`).
pub static UNMOVABLE_FULLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

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
/// found live; a regular expression: its compiled pattern (see
/// `Vm::prune_regexps`). (Not in [`trace`]: card scanning traces dead
/// cells too.)
#[inline(always)]
unsafe fn mark_function_code(p: usize, w: u64) {
    unsafe {
        if w & 1 != 0 {
            return;
        }
        let o = p as *const Object;
        match (*o).class {
            Class::Function => mark_code((*o).code),
            Class::RegExp if !(*o).code.is_null() => mark_regexp((*o).code as *const crate::regexp::Regex),
            _ => {}
        }
    }
}

/// Compiled pattern `re` found live in the marking under way, or used
/// during it.
#[inline(always)]
pub(crate) fn mark_regexp(re: *const crate::regexp::Regex) {
    unsafe { (*re).mark.store(code_epoch(), std::sync::atomic::Ordering::Relaxed) }
}

/// If the cell at `p` (header word `w`), marked, is an object: its shape
/// and those it was made from found live, their prototypes traced (see
/// `Shape::mark`). Shapes aren't roots: an object's shape and prototype
/// are kept by the object. (Not in [`trace`]: card scanning traces dead
/// cells too.)
#[inline(always)]
unsafe fn mark_object_shape(v: &mut impl Visit, w: u64, epoch: u32) {
    use std::sync::atomic::Ordering::Relaxed;
    if w & 1 != 0 {
        return;
    }
    let mut s = w as *const crate::object::Shape;
    unsafe {
        while !s.is_null() && (*s).mark.load(Relaxed) < epoch {
            (*s).mark.store(epoch, Relaxed);
            v.value((*s).proto);
            s = (*s).parent;
        }
    }
}

/// Live shape `s` and those it was made from: their prototypes marked,
/// once in the final pause of the marking `epoch` (see `Shape::traced`).
unsafe fn trace_shape(m: &mut impl Visit, mut s: *const crate::object::Shape, epoch: u32) {
    use std::sync::atomic::Ordering::Relaxed;
    unsafe {
        while !s.is_null() && (*s).traced.get() != epoch {
            (*s).traced.set(epoch);
            if (*s).mark.load(Relaxed) < epoch {
                (*s).mark.store(epoch, Relaxed);
            }
            m.value((*s).proto);
            s = (*s).parent;
        }
    }
}

/// What a full collection stopping the world, or a cycle's final pause,
/// moves (`CLOISON_GC_EVACUATE`, see `Vm::evacuate`).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) enum Evacuate {
    /// Nothing (`0`, `off`).
    Off,
    /// The cells of the sparsest blocks, up to a budget, when the small
    /// blocks hold more than `Policy::ratio` times what is live in them
    /// (the default).
    #[default]
    Fragmented,
    /// Every cell it may (`1`, `all`): the stress mode, which a reference
    /// no update reaches fails, reading [`POISON`].
    All,
}

/// When collections evacuate, and how much (`Vm::choose_candidates_for`,
/// `Vm::evacuate_from`), each from its environment variable, for trying
/// policies out.
pub(crate) struct Policy {
    /// Small blocks in use past this many times the bytes live in them
    /// (`CLOISON_GC_EVACUATE_RATIO`, 2).
    pub ratio: usize,
    /// The candidates give back at least these bytes
    /// (`CLOISON_GC_EVACUATE_MIN_MIB`, 32)...
    pub min: usize,
    /// ...and this percent of the small blocks in use, after a full
    /// collection stopping the world (`CLOISON_GC_EVACUATE_FULL_SHARE`, 10)
    /// or at a cycle's start (`CLOISON_GC_EVACUATE_SHARE`, 10). (At 35, as
    /// Splay's would give back 23-33%, Splay evacuates no more, nor the
    /// TypeScript and echarts apps, which then lose 11% and 15% off their
    /// peaks: see `interval`.)
    pub full_share: usize,
    pub share: usize,
    /// A block is a candidate under this percent full
    /// (`CLOISON_GC_EVACUATE_FULL`, 50).
    pub full: usize,
    /// Bytes a collection moves at most (`CLOISON_GC_EVACUATE_MIB`, 32).
    pub budget: usize,
    /// What the final pause's move must give back at least, or the next
    /// cycles choose none (`CLOISON_GC_EVACUATE_GAIN_MIB`, 8).
    pub gain: usize,
    /// A cycle chooses candidates only this long after the last cycle
    /// started (`CLOISON_GC_EVACUATE_INTERVAL_MS`, 500; 0: always), as V8's
    /// memory reducer compacts when allocation slows. Splay's cycles come a
    /// fifth of a second apart: evacuating at each took 5% off its peak and
    /// added 22% to its time (spaced out by their cost, `spacing`, its peak
    /// grew 6%: the copies take blocks at the final pause, the heap's peak).
    /// The web apps' come seconds apart.
    pub interval: std::time::Duration,
    /// ...and this many times as long after the last evacuation as it took
    /// (`CLOISON_GC_EVACUATE_SPACING`, 0: always).
    pub spacing: u32,
    /// Copies may take fresh blocks, not only those the heap has
    /// (`CLOISON_GC_EVACUATE_FRESH`, 1).
    pub fresh: bool,
}

/// Whether a cycle starting at `now` is too soon to choose candidates: the
/// last cycle started (`last_start`) less than `Policy::interval` before,
/// or the last evacuation ended (`last_end`, taking `took`) less than
/// `Policy::spacing` times as long as it took. (Every cycle's start counts,
/// one that chose none too: cycles a fifth of a second apart never
/// evacuate, however long they go on.)
fn too_soon(
    p: &Policy,
    now: std::time::Instant,
    last_start: Option<std::time::Instant>,
    last_end: Option<std::time::Instant>,
    took: std::time::Duration,
) -> bool {
    last_start.is_some_and(|t| now.duration_since(t) < p.interval) || last_end.is_some_and(|t| now.duration_since(t) < p.spacing * took)
}

pub(crate) fn policy() -> &'static Policy {
    static P: std::sync::OnceLock<Policy> = std::sync::OnceLock::new();
    P.get_or_init(|| {
        let n = |k: &str, d: usize| std::env::var(k).ok().and_then(|s| s.parse::<usize>().ok()).unwrap_or(d);
        Policy {
            ratio: n("CLOISON_GC_EVACUATE_RATIO", 2),
            min: n("CLOISON_GC_EVACUATE_MIN_MIB", 32) << 20,
            full_share: n("CLOISON_GC_EVACUATE_FULL_SHARE", 10),
            share: n("CLOISON_GC_EVACUATE_SHARE", 10),
            full: n("CLOISON_GC_EVACUATE_FULL", 50),
            budget: n("CLOISON_GC_EVACUATE_MIB", 32) << 20,
            gain: n("CLOISON_GC_EVACUATE_GAIN_MIB", 8) << 20,
            interval: std::time::Duration::from_millis(n("CLOISON_GC_EVACUATE_INTERVAL_MS", 500) as u64),
            spacing: n("CLOISON_GC_EVACUATE_SPACING", 0) as u32,
            fresh: n("CLOISON_GC_EVACUATE_FRESH", 1) != 0,
        }
    })
}

/// A moved cell's first granule's metadata while it is evacuated: its new
/// address in its second word (`Vm::evacuate`).
const FORWARDED: u8 = 0x40;
/// A candidate's cell's first granule's metadata while it is evacuated: it
/// stays (`Vm::evacuate`).
const PINNED: u8 = 0x10;

/// `f` of `items` in `threads` runs, each on its own thread but the first
/// (on this one), in order: on this thread alone if fewer than `min`.
fn in_parallel<I: Sync, T: Send>(threads: usize, items: &[I], min: usize, f: impl Fn(&[I]) -> T + Sync) -> Vec<T> {
    if threads < 2 || items.len() < min {
        return vec![f(items)];
    }
    let per = items.len().div_ceil(threads);
    let f = &f;
    std::thread::scope(|sc| {
        let rest: Vec<_> = items.chunks(per).skip(1).map(|c| sc.spawn(move || f(c))).collect();
        let mut r = vec![f(&items[..per])];
        r.extend(rest.into_iter().map(|h| h.join().unwrap()));
        r
    })
}

/// The cells marked with `color` in the small block at `start`, and their
/// sizes, from the metadata alone: a granule that isn't a marked cell's
/// first is skipped, eight at a time where they are zero, and a cell's
/// size is up to its `END` (set when it was traced: its header may not be
/// in cache).
unsafe fn marked_cells(start: usize, color: u8) -> Vec<(usize, usize)> {
    const ONES: u64 = 0x0101_0101_0101_0101;
    let end = start + crate::heap::BLOCK;
    // The metadata of the eight granules from `p`, those past the block's
    // zero (the next block's may not be mapped).
    let word = |p: usize| unsafe {
        if p + 8 * GRANULE <= end {
            (crate::heap::meta(p) as *const u64).read_unaligned()
        } else {
            let mut w = 0;
            for k in 0..(end - p) / GRANULE {
                w |= (*crate::heap::meta(p + k * GRANULE) as u64) << (8 * k);
            }
            w
        }
    };
    let mut cells = Vec::new();
    let mut p = start;
    while p < end {
        // (The bytes `color` in their `YOUNG | COLOR` bits: the lowest's top
        // bit is the lowest set in `z`. A zero byte isn't: colors aren't 0.)
        let w = word(p);
        let x = (w & ((YOUNG | COLOR) as u64 * ONES)) ^ (color as u64 * ONES);
        let z = x.wrapping_sub(ONES) & !x & (0x80 * ONES);
        if z == 0 {
            p += 8 * GRANULE;
            continue;
        }
        p += (z.trailing_zeros() / 8) as usize * GRANULE;
        if p >= start + crate::heap::BLOCK {
            break;
        }
        // (A cell ends in its block.)
        let mut q = p;
        loop {
            let e = word(q) & (END as u64 * ONES);
            if e != 0 {
                q += (e.trailing_zeros() / 8) as usize * GRANULE;
                break;
            }
            q += 8 * GRANULE;
        }
        let n = q + GRANULE - p;
        debug_assert_eq!(n, unsafe { cell_size(p as *const u8) });
        cells.push((p, n));
        p += n;
    }
    cells
}

/// Copy the cell at `p`, of `n` bytes, to `q`, and forward it there
/// ([`FORWARDED`] in its metadata, `q` in its second word).
unsafe fn copy_cell(p: usize, n: usize, q: usize) {
    unsafe {
        std::ptr::copy_nonoverlapping(p as *const u8, q as *mut u8, n);
        let first = *crate::heap::meta(p) & (COLOR | END);
        *crate::heap::meta(q) = first;
        *crate::heap::meta(q + n - GRANULE) |= END;
        let w = *(p as *const u64);
        if w & 1 == 0 {
            // A flat string's units are in it.
            let o = q as *mut Object;
            let old_units = p + crate::layout::slots_offset((*o).class);
            if (*o).class == Class::String && (*o).elems as usize == old_units {
                (*o).elems = (q + crate::layout::slots_offset((*o).class)) as *mut Value;
            }
        } else if w & 7 == UPVALUE && *((p + 8) as *const usize) == p + 16 {
            // A closed upvalue's variable is its own.
            *((q + 8) as *mut usize) = q + 16;
        }
        // (Its second word read above: now its new address.)
        *((p + 8) as *mut usize) = q;
        *crate::heap::meta(p) |= FORWARDED;
    }
}

/// A root as a collection that moves cells needs it (see
/// `Vm::each_root_slot`): a slot it may update, of a value (any: those
/// that aren't cells are skipped), an object pointer (null skipped) or an
/// array's or upvalue's payload pointer (past its header word; null
/// skipped), or a cell that must stay where it is, as a conservative word
/// refers to it or an address compiled code embeds.
#[derive(Clone, Copy)]
pub(crate) enum Root {
    Value(*mut Value),
    Object(*mut *mut Object),
    Payload(*mut usize),
    Pinned(usize),
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

    /// The value in slot `a`, a cell's word (see [`trace`]): [`Visit::value`]
    /// of it. A visitor that moves cells overrides it to update the slot.
    #[inline(always)]
    unsafe fn slot(&mut self, a: *mut Value) {
        unsafe { self.value(*a) }
    }

    /// The array's or upvalue's payload pointer in slot `a`, a cell's word:
    /// [`Visit::payload`] of it, overridden as [`Visit::slot`].
    #[inline(always)]
    unsafe fn payload_slot(&mut self, a: *mut usize) {
        unsafe { self.payload(*a) }
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
    /// The blocks evacuation candidates (see [`Candidates`]: those of a
    /// full collection stopping the world, or of the cycle under way), and
    /// the slots into them it traced.
    cand: Candidates,
    rec: Vec<usize>,
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
                        *m = (b & (END | EVAC)) | mode.color;
                        self.shade.push(p);
                    }
                }
                Kind::Old => {
                    if b & YOUNG != 0 || b & COLOR == 0 || b & COLOR == mode.color {
                        return;
                    }
                    // (A byte store: the mutator never writes an old cell's
                    // metadata while the marker runs.)
                    *m = (b & (END | EVAC)) | mode.color;
                    self.stack.push(p);
                }
                Kind::Full => {
                    if b & COLOR == mode.color {
                        return;
                    }
                    *m = (b & (END | EVAC)) | mode.color;
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
    /// As `Marking::cand` and `Marking::rec`.
    cand: Candidates,
    rec: Vec<usize>,
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
                Kind::MinorShade if b & COLOR != 0 && b & COLOR != mode.color => ((b & (END | EVAC)) | mode.color, true),
                Kind::Full if b & COLOR != mode.color => ((b & (END | EVAC)) | mode.color, false),
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
        unsafe {
            if self.cand.map != 0 { self.drain_to_in::<true>(stop, after) } else { self.drain_to_in::<false>(stop, after) }
        }
    }

    /// [`drain_to`](Self::drain_to), recording the slots into evacuation
    /// candidates if `R`.
    #[inline(always)]
    unsafe fn drain_to_in<const R: bool>(&mut self, stop: &dyn Fn() -> bool, after: usize) -> usize {
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
        let epoch = code_epoch();
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
                    // (A candidate's cell traced in a pause may move: a
                    // young one promoted, which only then gets `EVAC`. Not
                    // the marker's: the mutator may have stored one marked
                    // and without it into a cell traced already.)
                    if R && self.mode.kind != Kind::Old && self.cand.has(p) {
                        *crate::heap::meta(p) |= EVAC;
                    }
                    mark_function_code(p, w);
                    if self.mode.shapes {
                        mark_object_shape(self, w, epoch);
                    }
                    let size = cell_size(p as *const u8);
                    *crate::heap::meta(p + size - GRANULE) |= END;
                    self.marked += size;
                    n += 1;
                }
                if slots > SLICE_SLOTS {
                    let to = (from + SLICE_SLOTS).min(slots);
                    if R { trace(&mut Rec(self), p, p + 8 + 8 * from, p + 8 + 8 * to) } else { trace(self, p, p + 8 + 8 * from, p + 8 + 8 * to) };
                    if to < slots {
                        self.stack.push(to);
                        self.stack.push(p | 1);
                    }
                    work += SLICE_SLOTS / 32;
                } else {
                    if R { trace(&mut Rec(self), p, 0, usize::MAX) } else { trace(self, p, 0, usize::MAX) };
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
        let (mode, heap, cand) = (self.mode, self.heap, self.cand);
        let pool = &pool;
        let results: Vec<(usize, usize, Vec<usize>, Vec<usize>)> = std::thread::scope(|sc| {
            let mut deal = deal.into_iter();
            let first = (deal.next().unwrap(), slices);
            let helpers: Vec<_> = deal
                .map(|stack| {
                    sc.spawn(move || unsafe {
                        if cand.map != 0 {
                            trace_shared::<true>(pool, mode, heap, cand, stack, Vec::new())
                        } else {
                            trace_shared::<false>(pool, mode, heap, cand, stack, Vec::new())
                        }
                    })
                })
                .collect();
            let mut r = vec![unsafe {
                if cand.map != 0 {
                    trace_shared::<true>(pool, mode, heap, cand, first.0, first.1)
                } else {
                    trace_shared::<false>(pool, mode, heap, cand, first.0, first.1)
                }
            }];
            r.extend(helpers.into_iter().map(|h| h.join().unwrap()));
            r
        });
        let mut n = 0;
        for (cells, bytes, shade, rec) in results {
            n += cells;
            self.marked += bytes;
            self.shade.extend(shade);
            self.rec.extend(rec);
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
unsafe fn trace_shared<const R: bool>(
    pool: &TracePool,
    mode: Mode,
    heap: (usize, usize),
    cand: Candidates,
    stack: Vec<usize>,
    mut slices: Vec<(usize, usize)>,
) -> (usize, usize, Vec<usize>, Vec<usize>) {
    use std::sync::atomic::{AtomicU8, Ordering::Relaxed};
    let mut cm = CardMarking { mode, heap, stack, shade: Vec::new(), cand, rec: Vec::new() };
    let (mut n, mut bytes) = (0, 0);
    // As `drain`'s ring: cells prefetched `RING - 1` cells ahead.
    const RING: usize = 8;
    let mut ring = [0usize; RING];
    let (mut head, mut len) = (0, 0);
    let epoch = code_epoch();
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
                    if R && cand.has(p) {
                        (*(crate::heap::meta(p) as *const AtomicU8)).fetch_or(EVAC, Relaxed);
                    }
                    bytes += size;
                    n += 1;
                    let w = *(p as *const u64);
                    mark_function_code(p, w);
                    if mode.shapes {
                        mark_object_shape(&mut cm, w, epoch);
                    }
                    let slots = if w & 7 == VALUES { (w >> 32) as usize } else { 0 };
                    if slots > SLICE_SLOTS {
                        if R { trace(&mut Rec(&mut cm), p, p + 8, p + 8 + 8 * SLICE_SLOTS) } else { trace(&mut cm, p, p + 8, p + 8 + 8 * SLICE_SLOTS) };
                        slices.push((p, SLICE_SLOTS));
                    } else {
                        if R { trace(&mut Rec(&mut cm), p, 0, usize::MAX) } else { trace(&mut cm, p, 0, usize::MAX) };
                    }
                } else if let Some((p, from)) = slices.pop() {
                    let slots = (*(p as *const u64) >> 32) as usize;
                    let to = (from + SLICE_SLOTS).min(slots);
                    if R { trace(&mut Rec(&mut cm), p, p + 8 + 8 * from, p + 8 + 8 * to) } else { trace(&mut cm, p, p + 8 + 8 * from, p + 8 + 8 * to) };
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
    (n, bytes, cm.shade, cm.rec)
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
                        v.slot(a as *mut Value);
                        a += 8;
                    }
                }
                UPVALUE => {
                    if within(p + 16) {
                        v.slot((p + 16) as *mut Value);
                    }
                }
                // (Its header only: see `heap::WEAK_VALUES`.)
                WEAK_VALUES => {
                    let n = ((w >> 32) as usize).min(crate::collections::WEAK_HEADER);
                    let mut a = (p + 8).max(lo).next_multiple_of(8);
                    let last = (p + 8 + 8 * n).min(hi);
                    while a < last {
                        v.slot(a as *mut Value);
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
        let slots = p + crate::layout::slots_offset((*o).class);
        let mut a = slots.max(lo).next_multiple_of(8);
        let last = (slots + 8 * cap).min(hi);
        while a < last {
            v.slot(a as *mut Value);
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
                        v.slot(&raw mut (*o).props as *mut Value);
                    }
                    if within(code) {
                        v.slot(&raw mut (*o).code as *mut Value);
                    }
                } else if (*o).elems as usize != slots && within(elems) {
                    // Flattened: its units' buffer.
                    v.payload_slot(&raw mut (*o).elems as *mut usize);
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
            | Class::Symbol
            | Class::ArrayIterator
            | Class::IteratorHelper
            | Class::DisposableStack => {
                // The primitive it wraps (a string is a cell), or a
                // collection's entries array, or a promise's state.
                if within(code) {
                    v.slot(&raw mut (*o).code as *mut Value);
                }
            }
            Class::Generator | Class::Promise => {
                // Its state: a buffer of values (see `generator::state_of`,
                // `promise::cells`).
                if !(*o).code.is_null() && within(code) {
                    v.payload_slot(&raw mut (*o).code as *mut usize);
                }
            }
            Class::TypedArray | Class::DataView => {
                // `elems` points into the buffer's bytes.
                if within(code) {
                    v.slot(&raw mut (*o).code as *mut Value);
                }
                if within(props) {
                    v.payload_slot(&raw mut (*o).props as *mut usize);
                }
                return;
            }
            Class::ArrayBuffer => {
                // A resizable one's views (see `typed`).
                if !(*o).code.is_null() && within(code) {
                    v.slot(&raw mut (*o).code as *mut Value);
                }
            }
            Class::SharedArrayBuffer => {
                // A growable one's views; its bytes are no cell.
                if !(*o).code.is_null() && within(code) {
                    v.slot(&raw mut (*o).code as *mut Value);
                }
                if within(props) {
                    v.payload_slot(&raw mut (*o).props as *mut usize);
                }
                return;
            }
            Class::Function => {
                // After the in-object slots it was allocated with, as
                // `cell_size` has it: the marker thread reads no shape.
                let ups = (&(*(*o).code).upvalues).len();
                let up = (slots + 8 * cap) as *const usize;
                debug_assert_eq!(up as usize, crate::vm::Vm::upvalues(o) as usize);
                for k in 0..ups {
                    if within(up.add(k) as usize) {
                        v.payload_slot(up.add(k) as *mut usize);
                    }
                }
            }
            _ => {}
        }
        if within(props) {
            v.payload_slot(&raw mut (*o).props as *mut usize);
        }
        if within(elems) {
            v.payload_slot(&raw mut (*o).elems as *mut usize);
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
    /// The marking collects shapes (see `Mode::shapes`).
    shapes: bool,
    /// The cycle's evacuation candidates, and the slots into them traced
    /// (see [`Candidates`]), for the final pause.
    cand: Candidates,
    rec: Vec<usize>,
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
        let (color, heap, shapes, cand) = {
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
            (st.color, st.heap, st.shapes, st.cand)
        };
        let mut m = Marking {
            mode: Mode {
                color,
                kind: Kind::Old,
                check: false,
                shapes,
            },
            stack: &mut stack,
            shade: &mut shade,
            heap,
            marked: 0,
            cand,
            rec: Vec::new(),
        };
        unsafe { m.drain(&|| sh.pause.load(Relaxed)) };
        let marked = m.marked;
        let mut rec = std::mem::take(&mut m.rec);
        let mut st = sh.state.lock().unwrap();
        st.marked += marked;
        st.rec.append(&mut rec);
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

    /// A full collection now (`$262.gc()`): a concurrent cycle under way
    /// finished first, then a stop-the-world one and its sweep, so what
    /// died is free, and beyond the room kept, given back to the system.
    /// From a native: the running frame's registers are found through the
    /// native stack, as at a native's safepoint.
    pub(crate) fn collect_full_now(&mut self) {
        if self.heap.gc.stack_base == 0 {
            return;
        }
        let vm: *mut Vm = self;
        if self.heap.gc.marking {
            with_registers_spilled(&mut |sp| unsafe {
                (*vm).collect_from(sp, std::ptr::null_mut(), Collection::Final)
            });
        }
        self.heap.finish_sweep();
        with_registers_spilled(&mut |sp| unsafe { (*vm).collect_from(sp, std::ptr::null_mut(), Collection::Full) });
        self.heap.finish_sweep();
    }

    /// Enough shapes, or field cells, made for a full collection (see
    /// [`SHAPE_FULL_MIN`], [`CELL_FULL_MIN`]).
    pub(crate) fn shapes_full(&self) -> bool {
        let made = self.shapes_made();
        let cells = self.cells_made();
        made >= SHAPE_FULL_MIN && 2 * made >= self.shape_count()
            || cells >= CELL_FULL_MIN && 2 * cells >= self.cell_count()
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
            || self.shapes_full()
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
        self.short_strings.fill(Value::EMPTY);
        if matches!(kind, Collection::Start | Collection::Full) {
            self.heap.finish_sweep();
            // The cells that could still refer to them are swept.
            self.condemned.clear();
            self.free_condemned_shapes();
            self.heap.gc.codes_from = CODE_EPOCH.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            self.heap.gc.code_at_start = CODE_BYTES.with(|c| c.get());
            self.age_eval_cache();
            // Compiled code may give an object a shape a cache holds now,
            // and the cache drop it before the final pause: they are live.
            let made = self.shapes_made();
            self.heap.gc.shape_cycle = made >= (self.shape_count() / 8).max(SHAPE_SWEEP_MIN) || self.shapes_full();
            if kind == Collection::Start && self.heap.gc.shape_cycle {
                self.mark_cached_shapes();
            }
        } else {
            if self.heap.gc.sweeping {
                self.heap.absorb_sweep();
            }
            // (The sweep after a final pause is over: the shapes it
            // condemned go now, not at the next cycle's start.)
            if !self.heap.gc.sweeping {
                self.free_condemned_shapes();
            }
        }
        if kind == Collection::Start {
            self.heap.gc.cycle_from = self.heap.allocated();
            // (The colors of the last cycle's: its sweep is over.)
            if self.heap.gc.evacuate != Evacuate::Off {
                let gc = &self.heap.gc;
                let now = std::time::Instant::now();
                let early = gc.evacuate != Evacuate::All
                    && too_soon(policy(), now, gc.cycle_start, gc.evac_end, gc.evac_took);
                self.heap.gc.cycle_start = Some(now);
                if self.heap.gc.evac_wait > 0 || early {
                    self.heap.gc.evac_wait = self.heap.gc.evac_wait.saturating_sub(1);
                    self.heap.set_candidates(&[]);
                } else {
                    // (What is live: the last cycle's marking, not what the
                    // sweep left, as allocation has filled blocks since.)
                    let live = self.heap.gc.live;
                    self.choose_candidates_for(policy().share, Some(live));
                    self.heap.mark_evacuated();
                }
            }
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
            shapes: self.heap.gc.shape_cycle && !matches!(kind, Collection::Minor | Collection::MinorShade),
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
            // (The cycle's candidates: none outside one, but a full
            // collection's stopping the world.)
            cand: if kind == Collection::Minor { Candidates::default() } else { self.heap.candidates_map() },
            rec: Vec::new(),
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
            // (The weak collections' entries, once what is live is known:
            // in a full marking, with its live codes and shapes.)
            self.ephemerons(&mut m, sp, regs);
            if kind == Collection::Minor && self.heap.gc.check_barriers {
                self.heap.check_barriers();
            }
            // (Once marking is over: a full one's codes and shapes mark the
            // cells they refer to, a collection only a shape's prototype
            // holds say, whose keys must be pinned if it moves.)
            self.prune_collections(m.mode);
            if full {
                let words = self.stack_words(sp, regs);
                self.prune_regexps(m.mode, &words);
                self.prune_symbols(m.mode);
                let color = m.mode.color;
                self.prune_strings(|v| !v.is_cell() || *crate::heap::meta(v.to_bits() as usize) & COLOR == color);
            }
            if kind == Collection::Final && check {
                self.verify_marking(sp, regs, color);
            }
            // (Before anything is freed or condemned: compiled code dropped
            // next takes the cells it kept out of the roots.)
            if kind == Collection::Full && self.heap.gc.slots_check {
                self.check_root_slots(sp, regs, color);
            }
            let mut rec = std::mem::take(&mut m.rec);
            if full {
                if kind == Collection::Final {
                    rec.append(&mut self.heap.gc.evac_rec);
                    if let Some(mk) = &self.heap.gc.marker {
                        rec.append(&mut mk.shared.state.lock().unwrap().rec);
                    }
                }
                self.evacuate(sp, regs, color, rec);
            } else {
                self.heap.gc.evac_rec.append(&mut rec);
            }
            times[2] = lap();
            freed = match kind {
                // The young cells left unmarked are freed now, not by the
                // sweep: until their block is swept, a minor collection
                // would take a stale word to one for a live young cell and
                // keep it, with what it refers to swept from under it.
                Collection::Final => self.heap.free_young(check) + self.heap.start_sweep(color, check),
                Collection::Full => {
                    let f = self.heap.free_unmarked(color, check);
                    if self.heap.gc.evacuate != Evacuate::Off {
                        self.choose_candidates();
                    }
                    f
                }
                _ => self.heap.free_young(check),
            };
            if full {
                if self.heap.gc.shape_cycle {
                    self.sweep_shapes_after(color);
                    // (A stop-the-world collection's sweep is done: the
                    // shapes it condemned go now, not a collection later.
                    // del.js's unique shapes, one a `delete`, were then
                    // freed two full collections after they died.)
                    if kind == Collection::Full {
                        self.free_condemned_shapes();
                    }
                }
                self.condemn_codes();
                self.prune_atoms(self.heap.gc.codes_from);
                // (What the stacks hold, for the versions call sites left.)
                let words = self.stack_words(sp, regs);
                crate::jit::free_retired(self, &words);
            }
            if kind == Collection::Full && self.heap.gc.frag_report {
                self.frag_report(sp);
            }
            times[3] = lap();
        }
        let traced = m.marked;
        if full {
            self.heap.gc.promoted = 0;
        } else {
            self.heap.gc.promoted += traced;
        }
        if kind == Collection::Start {
            self.heap.gc.start_marked = traced;
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
            // and room to grow by what was live, times `GROWTH - 1`: what
            // this pause and the marker marked. (Not what the cycle's start
            // pause promoted, live too: the pacing was tuned without it, and
            // counting it grew Splay's peak RSS 4% at the same speed.)
            let live = traced + concurrent;
            let room = ((GROWTH - 1) * live).max(FULL_MIN);
            let start = if kind == Collection::Final { std::mem::take(&mut self.heap.gc.start_marked) } else { 0 };
            LIVE_PEAK.fetch_max((live + start) as u64, std::sync::atomic::Ordering::Relaxed);
            self.heap.gc.live = live;
            self.heap.gc.room = room;
            if kind == Collection::Final {
                self.heap.gc.lead = self.heap.allocated().saturating_sub(self.heap.gc.cycle_from);
            }
            // (After a final pause, again once the sweep has freed blocks.)
            self.heap.gc.full_at = self.heap.allocated() + self.heap.gc.headroom();
            self.heap.trim_empty();
        }
        if self.heap.gc.stats {
            let (top, recycle, free, large) = self.heap.block_counts();
            eprintln!(
                "gc {} {:?}: {:.3} ms (roots {:.3}, {} {:.3}, trace {:.3}, free {:.3}), {marked} cells marked ({} KiB), \
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
                traced >> 10,
                shade.len(),
                freed >> 10,
                self.heap.allocated() >> 20,
                self.heap.holes,
                self.heap.gc.marker.as_ref().map_or(0, |mk| mk.shared.state.lock().unwrap().sweep.len())
            );
            if full {
                self.heap.gc.report_room();
            }
            if self.heap.gc.shape_phases.any {
                self.heap.gc.shape_phases.report();
            }
        }
        self.heap.gc.shape_phases = ShapePhases::default();
        {
            use std::sync::atomic::Ordering::Relaxed;
            let end = std::time::Instant::now();
            let ns = (end - start).as_nanos() as u64;
            if !full {
                self.heap.gc.sample_minor(ns, end);
            }
            self.heap.gc.last_end = Some(end);
            PAUSE_NS.fetch_add(ns, Relaxed);
            MAX_PAUSE_NS.fetch_max(ns, Relaxed);
            ALLOCATED.fetch_add(self.heap.take_filled() as u64, Relaxed);
            if !full {
                PROMOTED.fetch_add(traced as u64, Relaxed);
            }
            match kind {
                Collection::Minor | Collection::MinorShade => MINORS.fetch_add(1, Relaxed),
                Collection::Full | Collection::Final => {
                    if self.unmovable > 0 {
                        UNMOVABLE_FULLS.fetch_add(1, Relaxed);
                    }
                    FULLS.fetch_add(1, Relaxed)
                }
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
                st.shapes = self.heap.gc.shape_cycle;
                st.cand = self.heap.candidates_map();
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
    /// live leave `weak_tables`. A full marking finds its live codes and
    /// shapes with them, before any entry is cleared: those mark cells
    /// (shapes' prototypes, what an inline cache's holder refers to), one
    /// a key or a collection (a typed array of a resizable buffer's views,
    /// a collection only a shape's prototype holds), and entries' values
    /// make codes live, until neither marks more (the review of #40). A
    /// minor collection keeps old cells, and the keys an old collection's
    /// entries refer to were marked from its dirty cards: it clears young
    /// collections' entries only.
    unsafe fn ephemerons(&mut self, m: &mut Marking, sp: usize, regs: *mut Value) {
        let full = m.mode.kind == Kind::Full;
        // The live collections, found as they become live: one may be
        // reached only through another's value (`m.set(k, new WeakMap)`).
        let mut tables: Vec<*mut Object> = Vec::new();
        let mut found = vec![false; self.weak_tables.len()];
        loop {
            if full {
                unsafe { self.mark_codes_and_shapes(m, sp, regs) };
            }
            if !unsafe { self.ephemeron_values(m, &mut found, &mut tables) } || !full {
                break;
            }
        }
        unsafe { self.ephemeron_clear(m.mode, &tables) };
        self.weak_tables = tables;
    }

    /// A full marking's live codes, and shapes if it collects them (see
    /// `mark_live_codes_and_shapes`).
    unsafe fn mark_codes_and_shapes(&mut self, m: &mut Marking, sp: usize, regs: *mut Value) {
        if self.heap.gc.shape_cycle {
            let mut t = std::time::Instant::now();
            unsafe { self.mark_live_codes_and_shapes(m, sp, regs) };
            self.heap.gc.shape_phases.mark += crate::gc::lap(&mut t);
            self.heap.gc.shape_phases.any = true;
        } else {
            unsafe { self.mark_live_codes(m, sp, regs, &mut Vec::new()) };
        }
    }

    /// Whether a cell at `p` is live for marking mode `mode`.
    fn ephemeron_live(mode: Mode, p: usize) -> bool {
        let b = unsafe { *crate::heap::meta(p) };
        match mode.kind {
            Kind::Minor | Kind::MinorShade => b != YOUNG,
            _ => b & COLOR == mode.color,
        }
    }

    /// The entries of weak collection `t`: its buffer, its length, and its
    /// entries' width.
    unsafe fn ephemeron_entries(t: *mut Object) -> Option<(*mut Value, usize, usize)> {
        unsafe {
            let e = Value::from_bits((*t).code as u64);
            if !e.is_cell() {
                return None;
            }
            let e = crate::object::as_object(e);
            let s = if matches!((*t).class, Class::WeakMap | Class::FinalizationRegistry) { 2 } else { 1 };
            Some(((*e).elems, (*e).elems_len as usize, s))
        }
    }

    /// [`ephemerons`]' marking: the collections that became live (`found`,
    /// by index in `weak_tables`) joined `tables`, the values of live keys
    /// marked, and in a full marking the symbols live shapes have as keys
    /// (which may be keys here, or lead to more: Codex on #40), until none
    /// is. Whether it marked one.
    unsafe fn ephemeron_values(&mut self, m: &mut Marking, found: &mut [bool], tables: &mut Vec<*mut Object>) -> bool {
        let mode = m.mode;
        let live = |p: usize| Self::ephemeron_live(mode, p);
        let mut any = false;
        unsafe {
            loop {
                let mut more = false;
                for (j, &t) in self.weak_tables.iter().enumerate() {
                    if !found[j] && live(t as usize) {
                        found[j] = true;
                        tables.push(t);
                    }
                }
                for &t in tables.iter() {
                    let Some((buf, len, 2)) = Self::ephemeron_entries(t) else { continue };
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
                if mode.kind == Kind::Full && self.mark_symbol_keys(m) {
                    more = true;
                }
                if !more {
                    break;
                }
                any = true;
                m.drain(&|| false);
            }
        }
        any
    }

    /// [`ephemerons`]' clearing: the entries of `tables` whose key isn't
    /// live.
    unsafe fn ephemeron_clear(&mut self, mode: Mode, tables: &[*mut Object]) {
        unsafe {
            for &t in tables {
                let Some((buf, len, s)) = Self::ephemeron_entries(t) else { continue };
                let mut i = crate::collections::BASE;
                while i + s <= len {
                    let k = *buf.add(i);
                    if k.is_cell() && !Self::ephemeron_live(mode, k.to_bits() as usize) {
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

    /// A full marking's codes: live if on a stack (the native one, or the
    /// VM's, as a frame's code), or marked since it began (a closure of it
    /// traced or made, it loaded), and with them the functions nested in
    /// them, which they make closures of. Mark what the live ones refer
    /// to, and what that reaches, until no more are. (A pinned one's
    /// references are marked anyway, see `mark_structures`, but it isn't
    /// live for that: one the JIT took stays for its address, and doesn't
    /// keep the functions nested in it, nor its compiled code, see
    /// `condemn_codes`. An eval'd function compiled, its enclosing one
    /// pinned to fold its variables, kept them and what they held: 2,000
    /// of them, each over a 160 KB array, took 376 MB.)
    /// `done`: the codes whose references are marked already, by index.
    unsafe fn mark_live_codes(&mut self, m: &mut Marking, sp: usize, regs: *mut Value, done: &mut Vec<bool>) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        unsafe {
            let stack = self.stack.as_ptr() as usize;
            let top = self.vm_stack_top(sp, regs);
            for a in (sp..self.heap.gc.stack_base).step_by(8).chain((stack..top).step_by(8)) {
                let w = *(a as *const usize);
                if self.code_set.contains(&w) {
                    mark_code(w as *const crate::vm::Code);
                }
            }
        }
        // While a lowering is under way, the codes the JIT has state or
        // decodings of are live: it may be inlining one, which no snapshot
        // names yet (and their constants, nested functions and compiled
        // code stay with them; the review of #39).
        if crate::jit::compiling() {
            for c in self.codes.iter() {
                if self.jit_caches.has(&**c) || c.jit.try_borrow().map_or(true, |j| j.is_some()) {
                    mark_code(&**c);
                }
            }
        }
        // (For the versions frames run, read once if any is asked for: see
        // `jit::mark_referenced_codes`.)
        let mut words: Option<Vec<usize>> = None;
        done.resize(self.codes.len(), false);
        loop {
            // The codes compiled code of live ones refers to, or of a frame.
            // (Only this VM's codes not freed: a version may outlive its
            // function, another VM's too.)
            let set = &self.code_set;
            let mut more = crate::jit::mark_referenced_codes(
                &mut || words.get_or_insert_with(|| unsafe { self.stack_words(sp, regs) }).clone(),
                &|c| set.contains(&(c as usize)) && unsafe { (*c).mark.load(Relaxed) } >= from,
                &mut |c| {
                    if !set.contains(&(c as usize)) {
                        return false;
                    }
                    let new = unsafe { (*c).mark.load(Relaxed) } < from;
                    mark_code(c);
                    new
                },
            );
            for (k, code) in self.codes.iter().enumerate() {
                if done[k] || code.mark.load(Relaxed) < from {
                    continue;
                }
                done[k] = true;
                more = true;
                for &c in code.closures.iter().filter(|c| !c.is_null()) {
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

    /// A full marking's codes and shapes (see `mark_live_codes`): a shape is
    /// live if an object traced has it or one made from it (see
    /// [`mark_object_shape`]), if it was made or given to an object since
    /// the marking began (`Vm::add_shape`, `object::set_shape`,
    /// `Vm::root_shape`), if a cache held it when the marking began (see
    /// `collect_from`), if it is pinned, or if the VM's tables, a live
    /// code's `new` or a store transition in its inline caches hold it.
    /// Live shapes' prototypes, and their ancestors', are marked, until no
    /// more codes or shapes are found live.
    unsafe fn mark_live_codes_and_shapes(&mut self, m: &mut Marking, sp: usize, regs: *mut Value) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        let epoch = code_epoch();
        let mut live = self.fixed_shapes();
        for i in 0..self.shape_count() {
            let s = self.shape_at(i);
            if s.pinned.get() || s.mark.load(Relaxed) >= from {
                live.push(s);
            }
        }
        let mut codes_done = vec![false; self.codes().count()];
        let mut ctors_done = vec![false; codes_done.len()];
        loop {
            for s in live.drain(..) {
                unsafe { trace_shape(m, s, epoch) };
            }
            unsafe { m.drain(&|| false) };
            unsafe { self.mark_live_codes(m, sp, regs, &mut codes_done) };
            for (k, code) in self.codes().enumerate() {
                if !ctors_done[k] && (code.pinned.get() || code.mark.load(Relaxed) >= from) {
                    ctors_done[k] = true;
                    for s in [code.ctor_shape.get(), code.ctor_result.get()] {
                        if !s.is_null() && s != crate::vm::MANY_SHAPES {
                            live.push(s);
                        }
                    }
                    // A store's transition: the shape a hit gives the
                    // object, not just compares with its own (a zombie's
                    // prototype is null), here and in code compiled from it.
                    for ways in code.made_ics().iter() {
                        for ic in unsafe { (*ways.get()).iter() } {
                            if !ic.new_shape.is_null() {
                                live.push(ic.new_shape);
                            }
                        }
                    }
                }
            }
            if live.is_empty() {
                break;
            }
        }
    }

    /// At the start of a marking: mark the shapes the tables compiled code
    /// reads hold, which it may give objects before the final pause even if
    /// the table drops them by then: the megamorphic store table's and
    /// `push` caches' transitions. (What they take in later comes with a
    /// shape given to an object, or made: see
    /// [`mark_live_codes_and_shapes`]. The interpreter takes inline
    /// caches' transitions through `object::set_shape`, compiled code
    /// those it embeds, pinned; `CreateObject`'s root shape, an inline
    /// cache's `shape`, is made again if it died, see `Vm::create_object`.)
    /// A marking that doesn't mark them first (a full one, stopping the
    /// world) may find them dead: `Vm::sweep_shapes` drops their entries.
    fn mark_cached_shapes(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let epoch = code_epoch();
        let mark = |s: *const crate::object::Shape| {
            if !s.is_null() && s != crate::vm::MANY_SHAPES && unsafe { (*s).mark.load(Relaxed) } < epoch {
                unsafe { (*s).mark.store(epoch, Relaxed) };
            }
        };
        for e in self.mega_store_entries() {
            mark(e.new_shape as *const crate::object::Shape);
        }
        for i in 0..self.shape_count() {
            let push = self.shape_at(i).push.get();
            if !push.is_null() {
                let c = unsafe { &*push };
                for &(_, t) in &c.to {
                    mark(t as *const crate::object::Shape);
                }
                mark(c.any as *const crate::object::Shape);
            }
        }
    }

    /// After a full marking that marked with `color`: the constructor
    /// tracking of prototypes it didn't mark goes, and the shapes it didn't
    /// find live become zombies (see `Vm::sweep_shapes`).
    fn sweep_shapes_after(&mut self, color: u8) {
        self.reset_shapes_made();
        let range = self.heap.cells_range();
        let mut t = std::time::Instant::now();
        self.sweep_ctor_protos(|p| is_cell_in(range, p) && unsafe { *crate::heap::meta(p) } & COLOR == color);
        self.heap.gc.shape_phases.ctor = lap(&mut t);
        let from = self.heap.gc.codes_from;
        self.sweep_shapes(from);
    }

    /// After a full marking, take the codes it didn't find live out of
    /// use: freed at the next collection that finishes the sweep first,
    /// their compiled code now, and what that kept (see `gc::KeptCells`);
    /// what the JIT found of them is forgotten (their addresses then
    /// others'). One the JIT took is live while compiled code alive refers
    /// to it (see `jit::mark_referenced_codes`): it was kept for good (#35).
    /// A native's stays (and, while a lowering is under way, one the JIT
    /// took is found live: see `mark_live_codes`). A pinned one not found
    /// live has no compiled code to drop.
    fn condemn_codes(&mut self) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        let (mut live, dead): (Vec<_>, Vec<_>) = std::mem::take(&mut self.codes)
            .into_iter()
            .partition(|c| c.pinned.get() || c.mark.load(Relaxed) >= from);
        // (A function freed is nested only in functions no marking found
        // live, which can't make a closure of it again; but the JIT's
        // analysis of a variable walks a pinned one's nested functions,
        // `jit::variable_assigned`: their entries become null. A freed one
        // can't assign the variable, so its answer stays right.)
        let mut gone = Vec::new();
        if !dead.is_empty() {
            let codes: crate::jit::HashSet<*const crate::vm::Code> =
                dead.iter().map(|c| &**c as *const crate::vm::Code).collect();
            for c in live.iter_mut() {
                for n in c.closures.iter_mut() {
                    if codes.contains(n) {
                        *n = std::ptr::null();
                    }
                }
            }
            self.jit_caches.forget(&codes);
            self.jit_caches.drop_retired_of(&codes);
            self.jitted.retain(|c| !codes.contains(c));
            // (And the field types they took: a field generalizing marks
            // its dependents' codes, and unbinds the calls into them.)
            let freed = |c: &usize| codes.contains(&(*c as *const crate::vm::Code));
            self.relied_cells.retain(|c, _| !freed(c));
            self.field_deps.retain(|_, deps| {
                deps.retain(|c| !freed(c));
                !deps.is_empty()
            });
            self.through_deps.retain(|_, calls| {
                calls.retain(|(caller, callee)| !freed(caller) && !freed(callee));
                !calls.is_empty()
            });
            for c in &dead {
                if let Ok(mut j) = c.jit.try_borrow_mut()
                    && let Some(mut j) = j.take()
                {
                    // (Versions calling each other hold each other.)
                    j.unbind_targets();
                    gone.push(j);
                }
            }
        }
        for c in live.iter().filter(|c| c.mark.load(Relaxed) < from) {
            if let Ok(mut j) = c.jit.try_borrow_mut()
                && let Some(j) = j.take()
            {
                gone.push(j);
            }
        }
        self.codes = live;
        drop(gone);
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

    /// The end of the deepest frame there may be on the VM stack, as
    /// `mark_roots` finds it: past `regs`, `osr_regs`, `native_top` and
    /// any VM stack address a native stack word holds (with `regs` null, a
    /// runtime call from compiled code, the frames are known only there).
    unsafe fn vm_stack_top(&self, sp: usize, regs: *mut Value) -> usize {
        let stack = self.stack.as_ptr() as usize;
        let stack_end = self.stack_end as usize;
        let mut top = (regs as usize).max(self.osr_regs as usize).max(self.native_top as usize);
        for a in (sp..self.heap.gc.stack_base).step_by(8) {
            let w = unsafe { *(a as *const usize) };
            if (stack..stack_end).contains(&w) {
                top = top.max(w);
            }
        }
        (top + 8 * self.max_frame).clamp(stack, stack_end)
    }

    /// The words of the native stack, from `sp`, and of the VM stack, to
    /// the end of the deepest frame there may be (as `mark_live_codes`
    /// reads them).
    unsafe fn stack_words(&self, sp: usize, regs: *mut Value) -> Vec<usize> {
        unsafe {
            let stack = self.stack.as_ptr() as usize;
            let top = self.vm_stack_top(sp, regs);
            (sp..self.heap.gc.stack_base).step_by(8).chain((stack..top).step_by(8)).map(|a| *(a as *const usize)).collect()
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
            // Symbols, by their key atoms (`Vm::symbols`): a full marking
            // keeps those live shapes have as keys (see `mark_symbol_keys`);
            // a minor collection all, while a marking runs the young ones
            // (an old one shaded would be live for the cycle).
            match m.mode.kind {
                Kind::Minor => self.symbols.values().for_each(|&o| m.value(crate::object::object_value(o))),
                Kind::MinorShade => {
                    for &o in self.symbols.values() {
                        let v = crate::object::object_value(o);
                        if self.heap.is_young(v) {
                            m.value(v);
                        }
                    }
                }
                _ => {}
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

    /// After a stop-the-world full collection (every cell left is live and
    /// old), for `CLOISON_GC_FRAG`: the small blocks by occupancy, in
    /// tenths, with the bytes of their cells that could not move were
    /// cells evacuated: those a native stack word may point into
    /// (conservative roots), those Rust structures and compiled code hold
    /// by address (`kept`, the cells the JIT's tables key by address), and
    /// collections' keys hashed by address. And what evacuating the
    /// movable cells of the blocks under half full would leave: the blocks
    /// with no pinned cell freed, their cells packed into blocks of their
    /// own.
    #[cold]
    #[inline(never)]
    unsafe fn frag_report(&mut self, sp: usize) {
        use crate::heap::BLOCK;
        const STACK: u8 = 1;
        const KEPT: u8 = 2;
        const HASHED: u8 = 4;
        let mut pin: std::collections::HashMap<usize, u8> = Default::default();
        let heap = &self.heap;
        let add = |pin: &mut std::collections::HashMap<usize, u8>, a: usize, why: u8| {
            if let Some(p) = heap.cell_containing(a) {
                *pin.entry(p).or_default() |= why;
            }
        };
        unsafe {
            for a in (sp..self.heap.gc.stack_base).step_by(8) {
                add(&mut pin, *(a as *const usize), STACK);
            }
        }
        for &a in self.heap.kept_addrs().iter().chain(&self.heap.gc.log_kept).chain(&self.relied_slots) {
            add(&mut pin, a, KEPT);
        }
        for &k in self.ctor_protos_keys().iter() {
            add(&mut pin, k, KEPT);
        }
        let ranges = self.heap.block_ranges();
        let cells = |f: &mut dyn FnMut(usize, usize)| {
            for &(start, end) in &ranges {
                let mut p = start;
                while p < end {
                    let m = unsafe { *crate::heap::meta(p) };
                    if m & (YOUNG | COLOR) == 0 {
                        p += crate::heap::GRANULE;
                        continue;
                    }
                    let n = unsafe { cell_size(p as *const u8) };
                    f(p, n);
                    p += n;
                }
            }
        };
        cells(&mut |p, _| {
            if unsafe { *(p as *const u64) } & 1 == 0 {
                crate::collections::address_keys(p as *mut crate::object::Object, &mut |k| add(&mut pin, k, HASHED));
            }
        });
        // By block: live bytes, pinned bytes (any reason), by reason.
        let mut blocks: std::collections::BTreeMap<usize, [usize; 5]> = Default::default();
        let mut large = 0;
        cells(&mut |p, n| {
            if !matches!(self.heap.block_kind(p), Some(true)) {
                large += n;
                return;
            }
            let b = blocks.entry(p & !(BLOCK - 1)).or_default();
            b[0] += n;
            let why = pin.get(&p).copied().unwrap_or(0);
            if why != 0 {
                b[1] += n;
            }
            for (k, w) in [STACK, KEPT, HASHED].into_iter().enumerate() {
                if why & w != 0 {
                    b[2 + k] += n;
                }
            }
        });
        let mut tenths = [[0usize; 6]; 10];
        let (mut freed, mut moved, mut kept_blocks) = (0, 0, 0);
        for b in blocks.values() {
            let t = &mut tenths[(b[0] * 10 / BLOCK).min(9)];
            t[0] += 1;
            for k in 0..5 {
                t[1 + k] += b[k];
            }
            if b[0] < BLOCK / 2 {
                moved += b[0] - b[1];
                if b[1] == 0 {
                    freed += 1;
                } else {
                    kept_blocks += 1;
                }
            }
        }
        let mib = |x: usize| x as f64 / (1 << 20) as f64;
        let total: usize = blocks.values().map(|b| b[0]).sum();
        let pinned: usize = blocks.values().map(|b| b[1]).sum();
        eprintln!(
            "frag: {} small blocks ({:.1} MiB) holding {:.1} MiB live ({:.0}% full), {:.2} MiB pinned \
             ({} cells: stack, kept, hashed below); large cells {:.1} MiB",
            blocks.len(),
            mib(blocks.len() * BLOCK),
            mib(total),
            100.0 * total as f64 / (blocks.len() * BLOCK).max(1) as f64,
            mib(pinned),
            pin.len(),
            mib(large)
        );
        for (i, t) in tenths.iter().enumerate() {
            if t[0] > 0 {
                eprintln!(
                    "  {:>3}-{:<3}% full: {:6} blocks, {:8.2} MiB live, pinned {:6.3} MiB (stack {:.3}, kept {:.3}, hashed {:.3})",
                    i * 10,
                    i * 10 + 10,
                    t[0],
                    mib(t[1]),
                    mib(t[2]),
                    mib(t[3]),
                    mib(t[4]),
                    mib(t[5])
                );
            }
        }
        let after = blocks.len() - freed + moved.div_ceil(BLOCK * 9 / 10);
        eprintln!(
            "  evacuating the blocks under half full: {:.1} MiB moved, {freed} blocks freed ({kept_blocks} kept by a pinned \
             cell), {} blocks -> {after} ({:.1} MiB -> {:.1})",
            mib(moved),
            blocks.len(),
            mib(blocks.len() * BLOCK),
            mib(after * BLOCK)
        );
        // The live cells by kind (an object's class), and the room objects
        // leave unused: in-object slots past their shape's, property and
        // element storage past what they hold.
        let mut kinds: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
        let (mut inline_unused, mut props_unused, mut elems_unused) = (0, 0, 0);
        cells(&mut |p, n| {
            let w = unsafe { *(p as *const u64) };
            let k = if w & 1 == 0 {
                let o = p as *const crate::object::Object;
                unsafe {
                    let used = (*(*o).shape).slots.len();
                    let inline = crate::object::Object::inline_cap(o);
                    inline_unused += 8 * inline.saturating_sub(used);
                    if !(*o).props.is_null() {
                        props_unused += 8 * (crate::object::Object::props_cap(o) as usize).saturating_sub(used.saturating_sub(inline));
                    }
                    if !(*o).elems.is_null() && !matches!(
                            (*o).class,
                            Class::String
                                | Class::TypedArray
                                | Class::DataView
                                | Class::ArrayBuffer
                                | Class::SharedArrayBuffer
                                | Class::BigInt
                                | Class::Temporal
                        ) {
                        elems_unused += 8 * (crate::object::Object::elems_cap(o) as usize).saturating_sub((*o).elems_len as usize);
                    }
                    format!("{:?}", (*o).class)
                }
            } else {
                match w & 7 {
                    crate::heap::VALUES => "values",
                    crate::heap::WEAK_VALUES => "weak values",
                    crate::heap::BYTES => "bytes",
                    crate::heap::UPVALUE => "upvalue",
                    _ => "?",
                }
                .to_string()
            };
            let e = kinds.entry(k).or_default();
            e.0 += 1;
            e.1 += n;
        });
        let mut kinds: Vec<_> = kinds.into_iter().collect();
        kinds.sort_by_key(|k| std::cmp::Reverse(k.1.1));
        let line: Vec<String> =
            kinds.iter().take(12).map(|(k, (c, b))| format!("{k} {:.1} MiB ({c})", mib(*b))).collect();
        eprintln!("  live by kind: {}", line.join(", "));
        eprintln!(
            "  unused room: in-object slots {:.1} MiB, property storage {:.1}, element storage {:.1}",
            mib(inline_unused),
            mib(props_unused),
            mib(elems_unused)
        );
    }

    /// Mark what Rust structures refer to: code, compiled code, the VM's
    /// tables, and shapes, unless the marking collects them (then they are
    /// found live: see `mark_live_codes_and_shapes`).
    unsafe fn mark_structures(&mut self, m: &mut Marking) {
        unsafe {
            // (Only the live structures': what compiled code dropped
            // embedded is not a root.)
            for a in self.heap.kept_addrs() {
                self.mark_interior(m, a);
            }
            self.roots_report(m, "kept");
            // (A marking that doesn't collect shapes keeps them all.)
            if !self.heap.gc.shape_cycle {
                for s in self.shapes() {
                    m.value(s.proto);
                }
            }
            // The other codes' references count once they are found
            // live (see `mark_live_codes`).
            for code in self.codes().filter(|c| c.pinned.get()) {
                self.mark_code_refs(m, code);
            }
            for v in self.registered_symbols() {
                m.value(v);
            }
            self.roots_report(m, "shapes, code, strings, symbols");
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
        let from = self.heap.gc.codes_from;
        let collecting = self.heap.gc.shape_cycle;
        let shape_live = |p: usize| unsafe {
            let w = *(p as *const u64);
            if collecting && w & 1 == 0 {
                let s = w as *const crate::object::Shape;
                assert!(
                    (*s).pinned.get() || (*s).mark.load(std::sync::atomic::Ordering::Relaxed) >= from,
                    "{p:#x} reachable, its shape {s:?} not live"
                );
            }
        };
        let mut r = Reach {
            seen: Default::default(),
            stack: Vec::new(),
            color,
        };
        unsafe {
            self.each_root(sp, regs, &mut |p| r.cell(p));
            while let Some(p) = r.stack.pop() {
                shape_live(p);
                trace(&mut r, p, 0, usize::MAX);
            }
        }
    }

    /// Move cells out of fragmented blocks, in a full collection stopping
    /// the world or a cycle's final pause, after it has marked and before
    /// anything is freed, and update every reference to them (stages 3 to
    /// 5 of evacuating fragmented blocks). What moves are the cells of the
    /// candidates (`Vm::choose_candidates`, `Heap::candidates`: chosen by
    /// the last full collection stopping the world, or at the cycle's
    /// start) marked [`EVAC`], but those pinned. The marking recorded the
    /// slots into them its cells hold (`Marking::cand`): a cycle's, those
    /// its start pause, its minor collections and the marker traced, cards
    /// included (a store of one into a cell traced already marks the card,
    /// [`EVAC`] being in the barrier's mask), and the final pause. So the
    /// update reads those, the roots' and the candidates' own cells, not the
    /// heap. No cell moves while Rust code holds copies of values
    /// (`Vm::unmovable`) or a function's JIT state is borrowed.
    ///
    /// Pinned: what the roots pin (native stack words, what Rust structures
    /// logged, what compiled code embeds), the VM's own cells, the cells
    /// holding prototype slots compiled code relies on, constructor
    /// tracking's prototypes, what the JIT compares or reads by address
    /// without keeping it (`jit::shape_words`), what the VM's tables key by
    /// address (`Vm::address_keyed_cells`), the keys collections hash by
    /// address (`Vm::collections`), and byte buffers (typed views point
    /// into them): [`PINNED`] while it runs. A moved cell is forwarded in place ([`FORWARDED`] in its
    /// metadata, its new address in its second word) until every reference
    /// is updated: the roots' slots (`each_root_slot`), the recorded ones,
    /// the candidates' cells' (`trace`), the weak collections and their
    /// entries, the collections' list, the state of a call being made,
    /// `typeof`'s strings, a conversion's pending error. Dropped: the lookup
    /// and megamorphic tables, the far keys' cache. The cells moved from are
    /// freed (and, stressed or verified, filled with [`POISON`], so a
    /// reference left reads that).
    #[cold]
    #[inline(never)]
    unsafe fn evacuate(&mut self, sp: usize, regs: *mut Value, color: u8, rec: Vec<usize>) {
        if self.heap.candidates.is_empty() {
            return;
        }
        let candidates: Vec<usize> = (self.heap.candidates.iter().enumerate())
            .filter(|&(_, &c)| c != 0)
            .map(|(b, _)| self.heap.base() + b * crate::heap::BLOCK)
            .collect();
        if self.unmovable == 0 {
            unsafe { self.evacuate_from(sp, regs, color, rec, &candidates) };
        }
        // (Whatever happened, they are blocks as any other now.)
        self.heap.set_candidates(&[]);
        self.heap.unmark_evacuated(&candidates);
    }

    /// [`Vm::evacuate`]'s move, from the blocks at `candidates`: of the
    /// cells marked [`EVAC`] (all those a full collection stopping the world
    /// marked there; those a cycle's start found there or its pauses
    /// promoted, whose every slot a cycle recorded).
    #[cold]
    #[inline(never)]
    unsafe fn evacuate_from(&mut self, sp: usize, regs: *mut Value, color: u8, rec: Vec<usize>, candidates: &[usize]) {
        use crate::heap::{BLOCK, BYTES, LARGE};
        let t0 = std::time::Instant::now();
        let mode = self.heap.gc.evacuate;
        // The candidates' cells marked.
        let threads = self.heap.gc.threads;
        let cells_in: Vec<(usize, usize)> =
            in_parallel(threads, candidates, 64, |bs| bs.iter().flat_map(|&b| unsafe { marked_cells(b, color) }).collect::<Vec<_>>())
                .concat();
        let t_live = t0.elapsed().as_secs_f64() * 1e3;
        // What moving them all would give back: the blocks they are in,
        // but their bytes. Little (the candidates' cells died since the
        // cycle chose them, as CyberChef's do), and the next cycles choose
        // none, for twice as many each time.
        if mode != Evacuate::All {
            let mut blocks = 0;
            let mut last = usize::MAX;
            for &(p, _) in &cells_in {
                if p / BLOCK != last {
                    last = p / BLOCK;
                    blocks += 1;
                }
            }
            let live: usize = cells_in.iter().map(|c| c.1).sum();
            if (blocks * BLOCK).saturating_sub(live) < policy().gain {
                self.heap.gc.fruitless(&format!("{} KiB live in {blocks} blocks", live >> 10));
                return;
            }
        }
        // Pins (those in the candidates): [`PINNED`] in their metadata.
        let cand = self.heap.candidates_map();
        let mut pins = 0usize;
        let mut pin = |p: usize| unsafe {
            let m = crate::heap::meta(p);
            if *m & PINNED == 0 {
                *m |= PINNED;
                pins += 1;
            }
        };
        unsafe {
            self.each_root_slot(sp, regs, &mut |r| {
                if let Root::Pinned(p) = r
                    && cand.has(p)
                {
                    pin(p);
                }
            })
        };
        // The VM's own cells, the global object and the intrinsics, a few
        // hundred: their addresses are copied around (the JIT's feedback
        // keeps the global object's).
        {
            let mut own: Vec<usize> = self.symbols.values().map(|&o| o as usize).collect();
            self.named_root_slots(&mut |r| unsafe {
                match r {
                    Root::Object(a) if !(*a).is_null() => own.push(*a as usize),
                    Root::Value(a) if (*a).is_cell() => own.push((*a).to_bits() as usize),
                    _ => {}
                }
            });
            for a in own {
                if cand.has(a) && self.heap.cell_containing(a) == Some(a) {
                    pin(a);
                }
            }
        }
        {
            let heap = &self.heap;
            // (A small cell is in one block. A word to a cell's start, as
            // most are, is found from its metadata alone.)
            let mut pin_word = |w: u64| {
                let w = w as usize;
                if !cand.has(w) {
                    return;
                }
                if w.is_multiple_of(GRANULE) && unsafe { *crate::heap::meta(w) } & (YOUNG | COLOR) != 0 {
                    pin(w);
                } else if let Some(p) = heap.cell_containing(w) {
                    pin(p);
                }
            };
            for &a in &self.relied_slots {
                pin_word(a as u64);
            }
            for k in self.ctor_protos_keys() {
                pin_word(k as u64);
            }
            if !crate::jit::shape_words(self, &mut pin_word) {
                return;
            }
            self.address_keyed_cells(&mut pin_word);
            // The keys collections hash by address.
            let cols: Vec<usize> = self.collections.iter().map(|&o| o as usize).collect();
            for keys in in_parallel(threads, &cols, 16, |cs| {
                let mut keys = Vec::new();
                for &o in cs {
                    crate::collections::address_keys(o as *mut Object, &mut |k| {
                        if cand.has(k) {
                            keys.push(k);
                        }
                    });
                }
                keys
            }) {
                for k in keys {
                    pin_word(k as u64);
                }
            }
        }
        let t_pins = t0.elapsed().as_secs_f64() * 1e3;
        // Copy, each cell moved forwarded in place: those that may move
        // (on several threads: their headers are read), up to the budget
        // (allocation may have filled the candidates), their new
        // addresses, then the copies (on several threads).
        let budget = if mode == Evacuate::All { usize::MAX } else { policy().budget };
        let mut skip = [0usize; 4];
        let mut from: Vec<(usize, usize)> = Vec::with_capacity(cells_in.len());
        for (f, k) in in_parallel(threads, &cells_in, 4096, |cs| {
            let (mut f, mut k) = (Vec::with_capacity(cs.len()), [0usize; 4]);
            for &(p, n) in cs {
                let m = unsafe { *crate::heap::meta(p) };
                let why = if n > LARGE {
                    0
                } else if m & EVAC == 0 {
                    1
                } else if m & PINNED != 0 {
                    2
                } else if unsafe { *(p as *const u64) } & 7 == BYTES {
                    3
                } else {
                    f.push((p, n));
                    continue;
                };
                k[why] += n;
            }
            (f, k)
        }) {
            from.extend(f);
            for i in 0..4 {
                skip[i] += k[i];
            }
        }
        let mut moved = 0;
        let fits = from.iter().take_while(|&&(_, n)| {
            moved += n;
            moved <= budget
        });
        let fits = fits.count();
        from.truncate(fits);
        // (Into blocks the heap has, stressed into fresh ones too.)
        let mut to: Vec<(usize, usize, usize)> = Vec::with_capacity(from.len());
        let (mut at, mut end) = (0usize, 0usize);
        for &(p, n) in &from {
            if at + n > end {
                let Some(b) = self.heap.evacuation_block(policy().fresh || mode == Evacuate::All) else { break };
                at = b;
                end = at + BLOCK;
            }
            to.push((p, n, at));
            at += n;
        }
        from.truncate(to.len());
        moved = from.iter().map(|c| c.1).sum();
        let cells = from.len();
        if from.is_empty() {
            return;
        }
        self.heap.gc.evac_backoff = 0;
        in_parallel(threads, &to, 4096, |cs| {
            for &(p, n, q) in cs {
                unsafe { copy_cell(p, n, q) };
            }
        });
        let t_copy = t0.elapsed().as_secs_f64() * 1e3;
        // Update.
        /// The candidates: a word outside them (a frame header's Rust
        /// address on the VM stack) is no cell moved, and has maybe no
        /// metadata to read (and those of cells not moved, most, aren't
        /// read: they miss the cache).
        struct Forward(Candidates);
        impl Forward {
            /// Where the cell at `p` went, if it moved (`p` in the range).
            #[inline(always)]
            unsafe fn at(p: usize) -> Option<usize> {
                unsafe { (*crate::heap::meta(p) & FORWARDED != 0).then(|| *((p + 8) as *const usize)) }
            }
            /// Where the cell at `p` went, if it is a candidate's and moved.
            #[inline(always)]
            fn to(&self, p: usize) -> Option<usize> {
                if !self.0.has(p) || p % GRANULE != 0 {
                    return None;
                }
                unsafe { Forward::at(p) }
            }
            #[inline]
            unsafe fn value(&self, a: *mut Value) {
                unsafe {
                    let v = *a;
                    if v.is_cell()
                        && let Some(q) = self.to(v.to_bits() as usize)
                    {
                        *a = Value::from_bits(q as u64);
                    }
                }
            }
            #[inline]
            unsafe fn object(&self, a: *mut *mut Object) {
                unsafe {
                    if !(*a).is_null()
                        && let Some(q) = self.to(*a as usize)
                    {
                        *a = q as *mut Object;
                    }
                }
            }
            #[inline]
            unsafe fn payload(&self, a: *mut usize) {
                unsafe {
                    let q = *a;
                    if q != 0
                        && let Some(n) = self.to(q - 8)
                    {
                        *a = n + 8;
                    }
                }
            }
        }
        impl Visit for Forward {
            unsafe fn cell(&mut self, _: usize) {}
            unsafe fn slot(&mut self, a: *mut Value) {
                unsafe { Forward::value(self, a) }
            }
            unsafe fn payload_slot(&mut self, a: *mut usize) {
                unsafe { Forward::payload(self, a) }
            }
        }
        let range = self.heap.cells_range();
        let fw = Forward(cand);
        unsafe {
            (*(self as *mut Vm)).each_root_slot(sp, regs, &mut |r| match r {
                Root::Value(a) => fw.value(a),
                Root::Object(a) => fw.object(a),
                Root::Payload(a) => fw.payload(a),
                Root::Pinned(_) => {}
            });
            // The slots into the candidates marking recorded, and the
            // candidates' own cells (moved, or left: pinned), on several
            // threads. (A slot recorded twice is written twice, the same.)
            // (Each slot by one thread: a slot may be recorded more than once,
            // by the marker and card scans, so the records go to a thread by
            // address.)
            let fw = &fw;
            let mut buckets: Vec<Vec<usize>> = vec![Vec::new(); if rec.len() >= 4096 { threads } else { 1 }];
            let k = buckets.len();
            for &a in &rec {
                buckets[(a >> 4) % k].push(a);
            }
            in_parallel(threads, &buckets, 2, |bs| {
                for &a in bs.iter().flatten() {
                    if a & 1 != 0 {
                        fw.payload((a & !1) as *mut usize);
                    } else {
                        fw.value(a as *mut Value);
                    }
                }
            });
            in_parallel(threads, &cells_in, 4096, |cs| {
                let mut fw = Forward(fw.0);
                for &(p, _) in cs {
                    let p = Forward::at(p).unwrap_or(p);
                    trace(&mut fw, p, 0, usize::MAX);
                }
            });
            for c in self.collections.iter_mut().chain(self.keyed_strings.iter_mut()) {
                fw.object(c);
            }
            // The weak collections, and their entries (traced only to their
            // header).
            for t in self.weak_tables.iter_mut() {
                fw.object(t);
            }
            for &t in &self.weak_tables {
                let e = Value::from_bits((*t).code as u64);
                if !e.is_cell() {
                    continue;
                }
                let e = crate::object::as_object(e);
                let (buf, len) = ((*e).elems, (*e).elems_len as usize);
                for i in crate::collections::BASE..len {
                    fw.value(buf.add(i));
                }
            }
            // A call being made.
            fw.object(&mut self.callee);
            for v in [&mut self.new_target, &mut self.native_new_target.0, &mut self.native_new_target.1, &mut self.derived_construct] {
                fw.value(v);
            }
            for env in self.eval_envs.iter_mut() {
                for v in env.iter_mut() {
                    fw.value(v);
                }
            }
            if let Some(crate::vm::Error(_, Some(e))) = &mut self.conv_error {
                fw.value(e);
            }
            // (Copies of string constants, which the table's slots have.)
            for v in self.type_names.iter_mut().flatten() {
                fw.value(v);
            }
        }
        let t_fix = t0.elapsed().as_secs_f64() * 1e3;
        if self.heap.gc.slots_check {
            unsafe { self.check_forwarded(sp, regs, color, range) };
        }
        // Caches of addresses.
        self.forget_lookups();
        self.far_keys.invalidate();
        // The cells moved from: free (and poisoned, stressed or verified).
        let poison = mode == Evacuate::All || self.heap.gc.checking();
        for &(p, n) in &from {
            unsafe {
                *crate::heap::meta(p) = 0;
                *crate::heap::meta(p + n - GRANULE) = 0;
                if poison {
                    std::ptr::write_bytes(p as *mut u8, POISON, n);
                }
            }
        }
        EVACUATED.fetch_add(moved as u64, std::sync::atomic::Ordering::Relaxed);
        self.heap.gc.evac_took = t0.elapsed();
        self.heap.gc.evac_end = Some(std::time::Instant::now());
        if self.heap.gc.stats {
            eprintln!(
                "evacuated {cells} cells ({} KiB) from {} blocks, {} slots recorded, {} pinned, in {:.1} ms (cells {t_live:.1}, pins {:.1}, copy {:.1}, update {:.1}); left KiB: large {}, no EVAC {}, pinned {}, bytes {}",
                moved >> 10,
                candidates.len(),
                rec.len(),
                pins,
                t0.elapsed().as_secs_f64() * 1e3,
                t_pins - t_live,
                t_copy - t_pins,
                t_fix - t_copy,
                skip[0] >> 10,
                skip[1] >> 10,
                skip[2] >> 10,
                skip[3] >> 10
            );
        }
    }

    /// `CLOISON_GC_SLOTS`, once an evacuation has updated what it found:
    /// panic on a slot of a root or of a live cell that still refers to a
    /// cell moved (a slot the cycle didn't record: a store without a
    /// barrier, say).
    #[cold]
    unsafe fn check_forwarded(&mut self, sp: usize, regs: *mut Value, color: u8, range: (usize, usize)) {
        struct Check(usize, usize, usize);
        impl Check {
            fn moved(&self, q: usize) -> bool {
                q >= self.0 && q < self.1 && q.is_multiple_of(GRANULE) && unsafe { *crate::heap::meta(q) } & FORWARDED != 0
            }
        }
        impl Visit for Check {
            unsafe fn cell(&mut self, _: usize) {}
            unsafe fn slot(&mut self, a: *mut Value) {
                let v = unsafe { *a };
                if v.is_cell() && self.moved(v.to_bits() as usize) {
                    panic!("slot {a:?} of {:#x} still refers to {:#x}, moved", self.2, v.to_bits());
                }
            }
            unsafe fn payload_slot(&mut self, a: *mut usize) {
                let q = unsafe { *a };
                if q != 0 && self.moved(q - 8) {
                    panic!("payload slot {a:?} of {:#x} still refers to {:#x}, moved", self.2, q - 8);
                }
            }
        }
        let mut c = Check(range.0, range.1, 0);
        unsafe {
            (*(self as *mut Vm)).each_root_slot(sp, regs, &mut |r| match r {
                Root::Value(a) => c.slot(a),
                Root::Object(a) => c.slot(a.cast()),
                Root::Payload(a) => c.payload_slot(a),
                Root::Pinned(_) => {}
            });
            for (start, end) in self.heap.block_ranges() {
                let mut p = start;
                while p < end {
                    let m = *crate::heap::meta(p);
                    if m & (YOUNG | COLOR) == 0 {
                        p += GRANULE;
                        continue;
                    }
                    let n = cell_size(p as *const u8);
                    if m & (YOUNG | COLOR) == color && m & FORWARDED == 0 {
                        c.2 = p;
                        trace(&mut c, p, 0, usize::MAX);
                    }
                    p += n;
                }
            }
        }
    }

    /// After a full collection stopping the world has swept (every cell
    /// left is live), or at a cycle's start (the last one's sweep over),
    /// the blocks the next final marking evacuates (`Heap::candidates`):
    /// when the small blocks in use are over
    /// `Policy::ratio` times the bytes live in them, the sparsest under
    /// `Policy::full`%, up to `Policy::budget` bytes, if they would give
    /// back at least `Policy::full_share`% of the blocks in use (and
    /// `Policy::min` bytes);
    /// stressed (`Evacuate::All`), every other block, alternately. Live
    /// bytes by block from what the sweep left (`Heap::swept_free`).
    fn choose_candidates(&mut self) {
        self.choose_candidates_for(policy().full_share, None);
    }

    /// [`Vm::choose_candidates`], giving back at least `Policy::min` bytes
    /// and `share` percent of the blocks in use; `live_bytes`, the bytes
    /// live, if not what the sweep left in the small blocks.
    fn choose_candidates_for(&mut self, share: usize, live_bytes: Option<usize>) {
        use crate::heap::BLOCK;
        // (Live bytes by block: what the sweep didn't free.)
        let mut live: Vec<(usize, usize)> = self.heap.swept_free.iter().map(|&(b, free)| (BLOCK - free.min(BLOCK), b)).collect();
        let chosen: Vec<usize> = if self.heap.gc.evacuate == Evacuate::All {
            self.heap.gc.evacuate_parity ^= true;
            let parity = self.heap.gc.evacuate_parity as usize;
            live.iter().filter(|&&(n, b)| n > 0 && (b / BLOCK) % 2 == parity).map(|&(_, b)| b).collect()
        } else {
            // (Blocks the sweep found full aren't listed: the room's those
            // listed, against the small blocks in use.)
            let in_use = self.heap.small_blocks() * BLOCK;
            let total: usize = live_bytes.unwrap_or_else(|| in_use - live.iter().map(|l| BLOCK - l.0).sum::<usize>());
            if in_use < policy().ratio * total {
                self.heap.set_candidates(&[]);
                return;
            }
            live.sort_unstable();
            let (mut budget, mut gain, mut chosen) = (policy().budget, 0, Vec::new());
            for &(n, b) in live.iter().filter(|&&(n, _)| n > 0 && n * 100 < BLOCK * policy().full) {
                if n > budget {
                    break;
                }
                budget -= n;
                gain += BLOCK - n;
                chosen.push(b);
            }
            if gain < policy().min.max(in_use * share / 100) {
                self.heap.set_candidates(&[]);
                return;
            }
            chosen
        };
        self.heap.set_candidates(&chosen);
    }

    /// The collections not live go from `Vm::collections`: a minor
    /// collection's, of those made since the last collection (it frees no
    /// other: going over all of them at each one doubled the TypeScript
    /// app's minor pauses).
    fn prune_collections(&mut self, mode: Mode) {
        let minor = matches!(mode.kind, Kind::Minor | Kind::MinorShade);
        let live = |p: usize| unsafe {
            let b = *crate::heap::meta(p);
            if minor { b != YOUNG } else { b & COLOR == mode.color }
        };
        let from = if minor { self.collections_young.min(self.collections.len()) } else { 0 };
        let mut young = self.collections.split_off(from);
        young.retain(|&o| live(o as usize));
        self.collections.append(&mut young);
        self.collections_young = self.collections.len();
        // The strings keeping their atom: a full marking's live ones' are
        // used (`Vm::prune_atoms` keeps them; a string asks no more).
        let from = if minor { self.keyed_young.min(self.keyed_strings.len()) } else { 0 };
        let mut young = self.keyed_strings.split_off(from);
        young.retain(|&o| live(o as usize));
        if !minor {
            let epoch = self.heap.gc.codes_from;
            for &o in &young {
                let a = unsafe { *Object::string_atom(o) } as usize;
                if let Some(u) = a.checked_sub(1).and_then(|a| self.atom_used.get_mut(a)) {
                    *u = epoch;
                }
            }
        }
        self.keyed_strings.append(&mut young);
        self.keyed_young = self.keyed_strings.len();
    }

    /// A full marking's shapes found (all, if it doesn't collect them):
    /// the symbols they have as keys are live (`Vm::symbols` holds them
    /// weakly: #33), what those refer to too; before the weak collections'
    /// dead entries go (see `ephemerons`), and once its codes and shapes
    /// are. Whether it marked one not marked yet.
    unsafe fn mark_symbol_keys(&mut self, m: &mut Marking) -> bool {
        use std::sync::atomic::Ordering::Relaxed;
        if self.symbols.is_empty() {
            return false;
        }
        let from = self.heap.gc.codes_from;
        let all = !self.heap.gc.shape_cycle;
        // (Which atoms are symbols', by index: a lookup per key took 11 ms
        // of each of PdfJS's full collections.)
        let mut symbol = vec![false; self.atoms.len()];
        for a in self.symbols.keys() {
            symbol[a.0 as usize] = true;
        }
        // (A running native's atoms too: its key list may name a symbol no
        // live object has any more, which it then reads, see
        // `Vm::held_atoms`.)
        let mut found: Vec<crate::object::Atom> =
            self.held_atoms.iter().copied().filter(|a| symbol.get(a.0 as usize) == Some(&true)).collect();
        self.shape_keys(
            |s| all || s.pinned.get() || s.mark.load(Relaxed) >= from,
            |a| {
                if symbol.get(a.0 as usize) == Some(&true) {
                    found.push(a);
                }
            },
        );
        let color = m.mode.color;
        let mut more = false;
        for a in found {
            if let Some(&o) = self.symbols.get(&a)
                && unsafe { *crate::heap::meta(o as usize) } & COLOR != color
            {
                unsafe { m.value(crate::object::object_value(o)) };
                more = true;
            }
        }
        unsafe { m.drain(&|| false) };
        more
    }

    /// After a full marking: forget the symbols it didn't find (their
    /// atoms then freed, see `Vm::prune_atoms`).
    fn prune_symbols(&mut self, mode: Mode) {
        self.symbols.retain(|_, &mut o| unsafe { *crate::heap::meta(o as usize) } & COLOR == mode.color);
    }

    /// After a full marking, before anything is freed or moved: drop the
    /// compiled patterns no live `RegExp` uses, nothing used during it and
    /// no frame holds (#34: a pattern made from each new source stayed for
    /// good), and the literals' entries keyed by a dead string (a freed
    /// code's constants: a new string at the address would find another's
    /// pattern) or naming a pattern dropped. `words` are the stacks' (see
    /// `stack_words`): a builtin holds its object's pattern across user
    /// code, which may `compile` the object another and collect (`exec`
    /// converting its input, `replace` calling back), the object then
    /// marking only the new one.
    fn prune_regexps(&mut self, mode: Mode, words: &[usize]) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        let live = |v: u64| !Value::from_bits(v).is_cell() || unsafe { *crate::heap::meta(v as usize) } & COLOR == mode.color;
        let size = std::mem::size_of::<crate::regexp::Regex>();
        let mut unmarked: Vec<usize> = self
            .regexps
            .values()
            .filter(|re| re.mark.load(Relaxed) < from)
            .map(|re| &**re as *const crate::regexp::Regex as usize)
            .collect();
        unmarked.sort_unstable();
        // (Addresses in a pattern, as `free_retired` finds the versions a
        // frame runs.)
        let held: crate::jit::HashSet<usize> = words
            .iter()
            .filter_map(|&w| {
                let i = unmarked.partition_point(|&p| p <= w).checked_sub(1)?;
                (w < unmarked[i] + size).then_some(unmarked[i])
            })
            .collect();
        self.regexps
            .retain(|_, re| re.mark.load(Relaxed) >= from || held.contains(&(&**re as *const crate::regexp::Regex as usize)));
        let kept: crate::jit::HashSet<*const crate::regexp::Regex> =
            self.regexps.values().map(|re| &**re as *const crate::regexp::Regex).collect();
        self.regexp_literals.retain(|&(p, f), re| live(p) && live(f) && kept.contains(re));
    }

    /// `CLOISON_GC_SLOTS`, after a full collection stopping the world: mark
    /// again from the roots' slots and pins alone ([`Vm::each_root_slot`]),
    /// through the cells' slots ([`trace`]) and the live weak collections'
    /// entries of live keys, and panic on a cell the collection marked
    /// that this didn't reach: one a moving collection would find no slot
    /// to update for (stage 2 of evacuating fragmented blocks).
    #[cold]
    #[inline(never)]
    unsafe fn check_root_slots(&mut self, sp: usize, regs: *mut Value, color: u8) {
        struct Reach {
            range: (usize, usize),
            color: u8,
            seen: std::collections::HashSet<usize>,
            work: Vec<usize>,
        }
        impl Reach {
            fn reach(&mut self, p: usize) {
                let marked = is_cell_in(self.range, p) && unsafe { *crate::heap::meta(p) } & (YOUNG | COLOR) == self.color;
                if marked && self.seen.insert(p) {
                    self.work.push(p);
                }
            }
        }
        impl Visit for Reach {
            unsafe fn cell(&mut self, p: usize) {
                self.reach(p)
            }
        }
        let range = self.heap.cells_range();
        let mut r = Reach { range, color, seen: Default::default(), work: Vec::new() };
        unsafe {
            self.each_root_slot(sp, regs, &mut |root| match root {
                Root::Value(a) => {
                    if (*a).is_cell() {
                        r.reach((*a).to_bits() as usize)
                    }
                }
                Root::Object(a) => r.reach(*a as usize),
                Root::Payload(a) => {
                    if *a != 0 {
                        r.reach(*a - 8)
                    }
                }
                Root::Pinned(p) => r.reach(p),
            });
            loop {
                while let Some(p) = r.work.pop() {
                    trace(&mut r, p, 0, usize::MAX);
                }
                // The weak collections' entries of keys reached.
                let mut more = false;
                for &t in &self.weak_tables {
                    if !r.seen.contains(&(t as usize)) || !matches!((*t).class, Class::WeakMap | Class::FinalizationRegistry) {
                        continue;
                    }
                    let e = Value::from_bits((*t).code as u64);
                    if !e.is_cell() {
                        continue;
                    }
                    let e = crate::object::as_object(e);
                    let (buf, len) = ((*e).elems, (*e).elems_len as usize);
                    let mut i = crate::collections::BASE;
                    while i + 1 < len {
                        let (k, v) = (*buf.add(i), *buf.add(i + 1));
                        if k.is_cell() && r.seen.contains(&(k.to_bits() as usize)) && v.is_cell() {
                            let before = r.work.len();
                            r.reach(v.to_bits() as usize);
                            more |= r.work.len() > before;
                        }
                        i += 2;
                    }
                }
                if !more {
                    break;
                }
            }
        }
        // Every cell left (all marked, all old): reached?
        let mut missed: Vec<(usize, String)> = Vec::new();
        let mut n = 0;
        for (start, end) in self.heap.block_ranges() {
            let mut p = start;
            while p < end {
                let m = unsafe { *crate::heap::meta(p) };
                if m & (YOUNG | COLOR) == 0 {
                    p += GRANULE;
                    continue;
                }
                let size = unsafe { cell_size(p as *const u8) };
                n += 1;
                if m & (YOUNG | COLOR) == color && !r.seen.contains(&p) {
                    let w = unsafe { *(p as *const u64) };
                    let kind = if w & 1 == 0 {
                        format!("{:?}", unsafe { (*(p as *const Object)).class })
                    } else {
                        format!("header {:#x}", w & 7)
                    };
                    missed.push((p, kind));
                }
                p += size;
            }
        }
        if self.heap.gc.stats || !missed.is_empty() {
            eprintln!("slots: {} cells, {} reached from the roots' slots, {} not", n, r.seen.len(), missed.len());
        }
        if !missed.is_empty() {
            for (p, k) in missed.iter().take(16) {
                eprintln!("  not reached: {p:#x} {k}");
            }
            // What refers to the first, up through what isn't reached either.
            struct Finds(usize, bool);
            impl Visit for Finds {
                unsafe fn cell(&mut self, p: usize) {
                    self.1 |= p == self.0;
                }
            }
            let mut target = missed[0].0;
            for _ in 0..6 {
                let mut by = None;
                for (start, end) in self.heap.block_ranges() {
                    let mut p = start;
                    while p < end && by.is_none() {
                        let m = unsafe { *crate::heap::meta(p) };
                        if m & (YOUNG | COLOR) == 0 {
                            p += GRANULE;
                            continue;
                        }
                        let mut f = Finds(target, false);
                        unsafe { trace(&mut f, p, 0, usize::MAX) };
                        if f.1 && p != target {
                            by = Some(p);
                        }
                        p += unsafe { cell_size(p as *const u8) };
                    }
                }
                let Some(b) = by else {
                    eprintln!("  {target:#x}: no cell refers to it (a Rust structure, then)");
                    self.who_holds(target, &r.seen);
                    break;
                };
                let w = unsafe { *(b as *const u64) };
                let kind = if w & 1 == 0 { format!("{:?}", unsafe { (*(b as *const Object)).class }) } else { format!("header {:#x}", w & 7) };
                eprintln!("  {target:#x} is referred to by {b:#x} {kind}, {}reached", if r.seen.contains(&b) { "" } else { "not " });
                if r.seen.contains(&b) {
                    break;
                }
                if b == missed[0].0 {
                    self.who_holds(target, &r.seen);
                    self.who_holds(b, &r.seen);
                    break;
                }
                target = b;
            }
            panic!("CLOISON_GC_SLOTS: {} marked cells no root's slot reaches", missed.len());
        }
    }

    /// `CLOISON_GC_SLOTS`' diagnosis: the Rust structures that hold `p`.
    #[cold]
    fn who_holds(&self, p: usize, _seen: &std::collections::HashSet<usize>) {
        use std::sync::atomic::Ordering::Relaxed;
        let from = self.heap.gc.codes_from;
        let v = p as u64;
        for s in self.shapes().chain(self.zombies.iter().map(|z| &**z)) {
            if s.proto.to_bits() == v {
                eprintln!("    the prototype of shape {:p}: mark {} (from {from}), pinned {}, unique {}, zombie {}", s, s.mark.load(Relaxed), s.pinned.get(), s.unique, s.is_zombie());
            }
        }
        for (k, c) in self.codes().chain(self.condemned.iter().map(|c| &**c)).enumerate() {
            if c.constants.iter().any(|x| x.to_bits() == v) {
                eprintln!("    a constant of code {k} {}: mark {}, pinned {}", c.name, c.mark.load(Relaxed), c.pinned.get());
            }
            for ways in c.made_ics().iter() {
                for ic in unsafe { (*ways.get()).iter() } {
                    if ic.holder as u64 == v {
                        eprintln!("    an inline cache's holder in code {k} {}: mark {}, pinned {}", c.name, c.mark.load(Relaxed), c.pinned.get());
                    }
                }
            }
        }
        if self.heap.kept_addrs().iter().any(|&a| self.heap.cell_containing(a) == Some(p)) {
            eprintln!("    kept");
        }
        if self.relied_slots.iter().any(|&a| self.heap.cell_containing(a) == Some(p)) {
            eprintln!("    a relied slot");
        }
        if self.ctor_protos_keys().contains(&p) {
            eprintln!("    a constructor-tracking key");
        }
        if self.named_roots().iter().any(|x| x.to_bits() == v) {
            eprintln!("    a named root");
        }
    }

    /// Every root of a full marking, as cells (`CLOISON_GC_VERIFY`): those of
    /// [`Vm::each_root_slot`].
    unsafe fn each_root(&mut self, sp: usize, regs: *mut Value, f: &mut dyn FnMut(usize)) {
        let range = self.heap.cells_range();
        let contains = |a: usize| a >= range.0 && a < range.1;
        unsafe {
            self.each_root_slot(sp, regs, &mut |r| match r {
                Root::Value(a) => {
                    let v = *a;
                    if v.is_cell() && is_cell_in(range, v.to_bits() as usize) {
                        f(v.to_bits() as usize)
                    }
                }
                Root::Object(a) => {
                    if !(*a).is_null() && contains(*a as usize) {
                        f(*a as usize)
                    }
                }
                Root::Payload(a) => {
                    if *a != 0 {
                        f(*a - 8)
                    }
                }
                Root::Pinned(p) => f(p),
            });
        }
    }

    /// Every root of a full marking as a slot it could update, or a cell
    /// that must stay (stage 2 of evacuating fragmented blocks): the VM
    /// stack's words and the arguments the runtime passes (`pending_args`),
    /// the VM's own references, field by field (`named_root_slots`), the
    /// handles, `WeakRef` targets and open upvalues, what Rust structures
    /// took since the last collection, the prototypes of the shapes it
    /// keeps, the constants and inline caches' holders of the functions it
    /// keeps, the string constants and `Symbol.for`'s symbols; pinned, what
    /// a native stack word points into and what compiled code embeds.
    unsafe fn each_root_slot(&mut self, sp: usize, regs: *mut Value, f: &mut dyn FnMut(Root)) {
        use std::sync::atomic::Ordering::Relaxed;
        unsafe {
            let stack = self.stack.as_ptr() as usize;
            let stack_end = self.stack_end as usize;
            let mut top = (regs as usize).max(self.osr_regs as usize).max(self.native_top as usize);
            for a in (sp..self.heap.gc.stack_base).step_by(8) {
                let w = *(a as *const usize);
                if (stack..stack_end).contains(&w) {
                    top = top.max(w);
                } else if let Some(p) = self.heap.cell_containing(w) {
                    f(Root::Pinned(p));
                }
            }
            let top = (top + 8 * self.max_frame).clamp(stack, stack_end);
            for a in (stack..top).step_by(8) {
                f(Root::Value(a as *mut Value));
            }
            let (args, n) = self.pending_args;
            if !args.is_null() {
                for i in 0..n {
                    f(Root::Value(args.add(i).cast_mut()));
                }
            }
            self.named_root_slots(f);
            // (The symbols a full marking found: see `mark_symbol_keys`.)
            for o in self.symbols.values_mut() {
                f(Root::Object(o));
            }
            for v in self.handles.iter_mut().chain(self.kept_alive.iter_mut()) {
                f(Root::Value(v));
            }
            for u in &mut self.open_upvalues {
                f(Root::Payload((u as *mut *mut crate::vm::Upvalue).cast()));
            }
            for &c in &self.heap.gc.log {
                f(Root::Pinned(c));
            }
            for &a in &self.heap.gc.log_kept {
                if let Some(p) = self.heap.cell_containing(a) {
                    f(Root::Pinned(p));
                }
            }
            for &a in &self.heap.gc.log_slots {
                f(Root::Object(a as *mut *mut Object));
            }
            let from = self.heap.gc.codes_from;
            let all = !self.heap.gc.shape_cycle;
            for s in self.shapes() {
                if all || s.pinned.get() || s.mark.load(Relaxed) >= from {
                    f(Root::Value((&raw const s.proto).cast_mut()));
                }
            }
            let heap = &self.heap;
            for code in self.codes().filter(|c| c.pinned.get() || c.mark.load(Relaxed) >= from) {
                for c in code.constants.iter() {
                    f(Root::Value((c as *const Value).cast_mut()));
                }
                for ways in code.made_ics().iter() {
                    for ic in (*ways.get()).iter_mut() {
                        // (A store's holder is a field type.)
                        if heap.contains(ic.holder as usize) {
                            f(Root::Object(&mut ic.holder));
                        }
                    }
                }
            }
            for v in self.string_constant_slots() {
                f(Root::Value(v));
            }
            for o in self.symbol_registry.values_mut() {
                f(Root::Object(o));
            }
            for a in self.heap.kept_addrs() {
                if let Some(p) = self.heap.cell_containing(a) {
                    f(Root::Pinned(p));
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
    /// The evacuation candidates for a marking ([`Mode::cand`]).
    fn candidates_map(&self) -> Candidates {
        if self.candidates.is_empty() {
            return Candidates::default();
        }
        Candidates { map: self.candidates.as_ptr() as usize, len: self.candidates.len(), base: self.base() }
    }

    /// At a cycle's start: [`EVAC`] on the old cells of the evacuation
    /// candidates, the cells the final pause may move (with the young ones
    /// the cycle's pauses promote there, see `Marking::drain_to_in`).
    fn mark_evacuated(&mut self) {
        self.each_candidate(|b| if b & COLOR != 0 && b & YOUNG == 0 { b | EVAC } else { b });
    }

    /// Once the final pause has moved what it could: no cell keeps [`EVAC`]
    /// (nor [`PINNED`]).
    fn unmark_evacuated(&mut self, blocks: &[usize]) {
        for &a in blocks {
            Self::each_meta(a, |b| b & !(EVAC | PINNED));
        }
    }

    /// `f` of each first granule's metadata byte of the candidates'.
    fn each_candidate(&mut self, f: impl Fn(u8) -> u8 + Copy) {
        let base = self.base();
        for (b, &c) in self.candidates.iter().enumerate() {
            if c != 0 {
                Self::each_meta(base + b * crate::heap::BLOCK, f);
            }
        }
    }

    /// Each nonzero metadata byte of the block at `a` replaced by `f` of it,
    /// eight at a time where they are zero.
    fn each_meta(a: usize, f: impl Fn(u8) -> u8) {
        let (lo, hi) = (crate::heap::meta(a) as usize, crate::heap::meta(a + crate::heap::BLOCK) as usize);
        let mut m = lo;
        unsafe {
            while m < hi {
                if (m as *const u64).read_unaligned() == 0 {
                    m += 8;
                    continue;
                }
                for k in m..m + 8 {
                    let b = *(k as *const u8);
                    if b != 0 {
                        *(k as *mut u8) = f(b);
                    }
                }
                m += 8;
            }
        }
    }

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
            // (The slots into evacuation candidates recorded, through
            // `Rec`, only while a cycle has some.)
            let cand = m.cand;
            if threads < 2 || cards < PARALLEL_CARDS {
                let mut refs = CardRefs::new(m);
                for &(lo, hi) in &runs {
                    if cand.map != 0 {
                        self.scan_card(lo, hi, &mut Rec(&mut refs));
                    } else {
                        self.scan_card(lo, hi, &mut refs);
                    }
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
                    let mut cm = CardMarking { mode, heap: range, stack: Vec::new(), shade: Vec::new(), cand, rec: Vec::new() };
                    let mut refs = CardRefs::new(&mut cm);
                    loop {
                        let i = next.fetch_add(RUNS_PER_TAKE, std::sync::atomic::Ordering::Relaxed);
                        if i >= runs.len() {
                            break;
                        }
                        for &(lo, hi) in &runs[i..(i + RUNS_PER_TAKE).min(runs.len())] {
                            if cand.map != 0 {
                                (*heap.0).scan_card(lo, hi, &mut Rec(&mut refs));
                            } else {
                                (*heap.0).scan_card(lo, hi, &mut refs);
                            }
                        }
                    }
                    refs.flush();
                    (cm.stack, cm.shade, cm.rec)
                };
                std::thread::scope(|sc| {
                    let helpers: Vec<_> = (1..threads).map(|_| sc.spawn(work)).collect();
                    let mut take = |(stack, shade, rec): (Vec<usize>, Vec<usize>, Vec<usize>)| {
                        m.stack.extend(stack);
                        m.shade.extend(shade);
                        m.rec.extend(rec);
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
        self.swept_free.clear();
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
            if self.gc.stats {
                let [partly, free, recycled, empty] = std::mem::take(&mut self.swept);
                eprintln!(
                    "  swept: {partly} blocks with cells ({} MiB, {} MiB of it free, {recycled} to reuse), \
                     {empty} emptied; {} MiB of blocks in use, {} MiB kept empty, next cycle at {} MiB",
                    (partly * crate::heap::BLOCK) >> 20,
                    free >> 20,
                    self.allocated() >> 20,
                    (self.kept_empty() * crate::heap::BLOCK) >> 20,
                    self.gc.full_at >> 20
                );
            }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn policy_with(interval_ms: u64, spacing: u32) -> Policy {
        Policy {
            ratio: 2,
            min: 32 << 20,
            full_share: 10,
            share: 10,
            full: 50,
            budget: 32 << 20,
            gain: 8 << 20,
            interval: Duration::from_millis(interval_ms),
            spacing,
            fresh: true,
        }
    }

    /// The first cycle, cycles too close, one far enough after, no
    /// interval, and the spacing after an evacuation.
    #[test]
    fn cycles_choose_candidates_only_far_enough_apart() {
        let t0 = Instant::now();
        let at = |ms| t0 + Duration::from_millis(ms);
        let p = policy_with(500, 0);
        let none = Duration::ZERO;
        assert!(!too_soon(&p, at(0), None, None, none), "the first cycle");
        assert!(too_soon(&p, at(200), Some(at(0)), None, none), "200 ms after the last");
        assert!(!too_soon(&p, at(600), Some(at(100)), None, none), "500 ms after");
        // Each start counts: cycles 200 ms apart never get there.
        let mut last = at(0);
        for k in 1..50 {
            assert!(too_soon(&p, at(200 * k), Some(last), None, none));
            last = at(200 * k);
        }
        let p = policy_with(0, 0);
        assert!(!too_soon(&p, at(1), Some(at(0)), None, none), "no interval");
        let p = policy_with(0, 20);
        let took = Duration::from_millis(40);
        assert!(too_soon(&p, at(700), Some(at(0)), Some(at(0)), took), "within 20 times 40 ms");
        assert!(!too_soon(&p, at(900), Some(at(0)), Some(at(0)), took), "past it");
    }
}
