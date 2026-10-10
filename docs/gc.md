# The garbage collector

Status: design, 2026-10-10. Nothing implemented. Supersedes the collector
parts of `docs/object-model.md` (its escape analysis and compact layouts
stand); fits the reservation and offset plan of `docs/sandbox.md`.

## Decision

A **generational collector with precise roots, concurrent on the old
space, parallel on the nursery**:

- a copying nursery, collected in short stop-the-world pauses that helper
  threads share (V8's parallel Scavenger), with promotion by age;
- a mark-region old space (Immix's design: blocks of lines, allocation
  by bumping into free runs of lines), marked and swept by collector
  threads while the mutator runs, with two brief pauses per cycle (take
  the roots, finish the roots), and evacuation of the sparsest blocks in
  the second pause when fragmentation is measured, within a budget;
- a non-moving space for large cells and buffers;
- one reservation for all of it, as the sandbox wants;
- one write barrier, three instructions on the hot path, serving both the
  remembered set and concurrent marking;
- pacing from measurement: the nursery size, the old-space headroom and
  the moment marking starts come from the costs the collector observes,
  not from presets.

Collections run only at safepoints, where every live value is in a VM
frame, a handle or a named VM root, so Rust runtime code needs no handles
unless it calls back into JS.

Why these, against the alternatives studied below:

- **Concurrent marking and sweeping, parallel copying, no concurrent
  copying.** On the heaps that matter (pdfjs 1.6 GB, EarleyBoyer 4.6 GB
  allocated, TypeScript's growing structures) marking the old space is the
  dominant cost, and it can run entirely on other cores: JSC's Riptide and
  V8's Orinoco both took main-thread marking down by two thirds or more
  ([Riptide][riptide], [concurrent marking][v8-cm]). Sweeping is
  embarrassingly concurrent with side mark bits. The nursery is different:
  copying objects while the mutator runs needs a read barrier on every
  heap load (ZGC's coloured pointers, Shenandoah's load-reference barrier),
  which is incompatible with raw NaN-boxed pointers and with compiled code
  that folds loads, and which no JavaScript engine does. So minor
  collections stop the world, but their cost is survivors not garbage,
  and helper threads copy alongside the mutator; V8 measured 20 to 50% off
  the main-thread share ([Orinoco][orinoco]). The ckronos engine
  (`~/git/js-interpreter`) is a data point for the split: the same Splay
  run took 0.36 s with its concurrent marker and did not finish in 120 s
  with it disabled, measured here.
- **Copying nursery, not a non-moving or mark-in-place one.** Allocation
  stays a bump (what `construct_direct` and the `CreateObject` stencils do
  today), and dead young objects, the bulk of what Octane allocates, cost
  nothing. V8 built a non-moving young generation (MinorMS) and found it "a
  lose relative to the copying nursery"; its sticky-mark-bit generational
  mode "didn't work out" either ([Wingo 2025][v8-2025]). ckronos's
  non-moving region nursery reclaimed 18% of EarleyBoyer's memory and none
  of Splay's.
- **Precise roots, not a conservative stack scan.** JSC scans the machine
  stack conservatively because three JITs and C++ hold raw pointers, and
  that is why it can never move anything. toi's compiled code already
  writes every live VM register back, boxed, at exits and before runtime
  calls, and knows at each site which machine registers hold boxed values;
  the interpreter's frames are arrays of `Value`. The roots are precise
  already; the collector only has to read them.
- **A mark-region old space that rarely needs to move anything, and
  moves a bounded amount when it does.** A heap of size-classed blocks
  (JSC's) has one pathology: a block serves one size, so after a phase
  change a block that is dead but for one 64-byte object can serve no
  128-byte allocation, and in the worst case every live object pins a
  block. That is what makes compaction necessary in such heaps, and JSC
  lives without it by accepting the memory. In a mark-region heap
  ([Immix][immix]) a block has no size class: allocation bumps into any
  free run of 128-byte lines, so the memory a live object can waste is
  bounded by the line it sits in, not by a block, and a phase change
  leaves holes any size can fill. That bound is usually enough; when the
  sweep measures that it is not, the next cycle evacuates the sparsest
  blocks into holes elsewhere, a budgeted amount per cycle, in the same
  pause that finishes marking, on the slots the concurrent marker recorded
  as pointing into them (V8's evacuation candidates, LXR's pattern of
  concurrent marking with stop-the-world copying). Objects that compiled
  code, inline caches or shapes refer to are *pinned* per cell and stay,
  so nothing outside the heap is ever patched, and nothing moves while the
  markers run, which is what keeps concurrent marking simple: V8's marker
  must snapshot objects whose layout changes under it because it compacts
  concurrently with layout changes; here moving happens only in a pause.
- **Generations, not the internal/external split.** `object-model.md`
  proposed two heaps separated by observability, with transitive
  externalization on a store into an external object. A generational heap
  gives the same three things it was after: minor collections trace only
  from frames (plus the remembered set, which is small), dead young graphs
  are freed in bulk, and stores into young objects carry no barrier. It
  does not pay the transitive walk, whose cost is unbounded, and it does
  not need the invariant that no external object points to an internal
  one, which every builtin would have had to respect. What the split had
  over generations, keeping a graph that lives for one long call out of
  the old space, a generational heap recovers partly with pretenuring and
  a nursery sized to the survival rate, and the concurrent marker makes
  the rest cheap: a graph promoted and then dropped costs marking time on
  another core, not mutator time. The escape analysis of `object-model.md`
  (virtual objects, compact layouts) is orthogonal and stays.

## Numbers the design is for

Octane's deterministic runs on toi today (2026-10-10, Apple M-series,
`target/release/toi`, peak RSS; with no collector that is the allocation
volume):

| benchmark | allocated | time | rate |
|---|---:|---:|---:|
| Splay | 4.9 GB | 2.6 s | 1.9 GB/s |
| EarleyBoyer | 4.65 GB | 27.8 s | 170 MB/s |
| RegExp | 2.5 GB | 18.8 s | 130 MB/s |
| pdfjs | 1.6 GB | 2.4 s | 670 MB/s |
| RayTrace | 1.37 GB | 11.6 s | 120 MB/s |
| DeltaBlue | 832 MB | 1.2 s | 700 MB/s |
| Crypto | 772 MB | 5.7 s | 135 MB/s |
| code-load | 569 MB | 2.7 s | 210 MB/s |
| TypeScript | 534 MB | 4.1 s | 130 MB/s |
| Box2D | 228 MB | 1.7 s | |
| GBEmu | 189 MB | 1.2 s | |
| Richards | 47 MB | 1.6 s | |
| NavierStokes | 10 MB | 7.1 s | |

The three largest ran several times slower than the README's Linux figures
here: the machine was paging. That is the first thing the collector buys.

Two shapes of workload: DeltaBlue, Richards, RayTrace and Splay allocate
small objects at hundreds of MB/s with most dying young (Splay's tree
nodes are the exception that generational collectors are tested on), while
pdfjs, TypeScript and code-load build large structures that live until the
end. The nursery pays for the first group; the old space's headroom
policy and the concurrent marker decide the second.

Header cost: a DeltaBlue `Variable` has 7 fields, 56 bytes of slots after
a 48-byte header. JSC's header is 8 bytes, Java's 12 (8 with
[JEP 519][lilliput], measured at 22% less heap). The header is the largest
memory lever after the collector itself; see "Later".

## What others do

### Lua

A single `marked` byte per object carries colour, age and finalization
state; the heap is a linked list. The collector is incremental tri-colour
mark-sweep with two write barriers: a *forward* barrier marks the stored
value (used for metatables and closed upvalues, which are small and
stable), a *back* barrier turns the black *table* grey again and puts it on
`grayagain`, to be traversed once more in the atomic phase, so a loop
filling a table costs one re-traversal, not a mark per store. Stacks are
never barriered: they are kept grey and rescanned at the end. Open
upvalues are grey and barrier-free; closing one is the only point that
checks. The 5.4 generational mode reuses the back barrier as its remembered
set ("touched" objects), needs two cycles before an object is old (5.2's
one-cycle version promoted everything), falls back to a full collection
when a minor one reclaims too little, and stayed off by default because
"programs which build large data structures won't benefit". 5.5 (2025)
makes its majors incremental and paces on bytes marked instead of bytes
allocated, which its authors called "mystifying" as a clock.
([lgc.c][lgc], [Ierusalimschy 2018][lua-ws18])

Taken: collect only where the stack is consistent, no barrier on frames,
the open-upvalue rule, an interning table that is never a root, the
nursery as one grey object rescanned at the end. Not taken: the
header-linked list (every sweep is O(objects)), bytes allocated as the
pacer, and single-threadedness.

### JavaScriptCore

Non-moving mark-sweep in 16 KB `MarkedBlock`s of one cell size each
(16-byte atoms, sizes 16…80 then ×1.4), mark bits in a per-block bitmap
cleared for free by a version number, allocation by *bump'n'pop* over
intervals of free cells that the sweep builds lazily, large cells
malloc'ed. Generational by sticky mark bits. Riptide marks concurrently
with parallel markers and an object-granular *retreating wavefront*
barrier: `store; if (cell.state <= threshold) slow`, where the slow path
turns the object grey again and pushes it on the mutator's mark stack; the
threshold is a VM variable, so the barrier is a byte compare that does
nothing when no collection is running, and during marking the slow path
pays a store-load fence. Marking runs through "constraints" solved to a
fixpoint (weak maps, inline caches, code blocks holding structures weakly
and jettisoned when one dies); the world stops only to take roots and to
converge, with a space-time scheduler that stops the mutator if the
collector falls behind. Roots are a conservative scan of every thread's
stack. The header is 8 bytes: a 32-bit `StructureID` and 32 bits of type,
flags and cell state. Riptide's numbers: Splay's worst pauses from over
10 ms to under 3 ms, typical pauses at or under 1 ms, about 5% overhead
during a collection and none outside one. ([Riptide][riptide],
[Understanding GC in JSC][jsc-2022], `heap/MarkedBlock.h`,
`heap/Collector.cpp`)

Taken: side mark bits with version clearing, bump allocation over free
intervals (JSC's intervals are Immix's holes), concurrent lazy sweeping,
the two-pause concurrent cycle, the threshold trick that lets one barrier
switch modes, code holding heap constants with an invalidation rule, the
collector pacing against mutator progress. Not taken: size-classed
blocks (the fragmentation argument above), conservative scanning, the
object-granular barrier (below), IsoSubspaces, the fenced slow path
(single mutator: no fence).

### V8

Semi-space Scavenger for the young generation (256 KB pages, promotion on
the second survival through a per-page *age mark*, parallel copying with
CAS on the forwarding word), mark-compact old space with incremental and
concurrent marking, parallel evacuation of the most fragmented pages, lazy
and concurrent sweeping. The write barrier tests a page flag on the host
("pointers from here are interesting") and on the value ("pointers to
here"); the remembered set is a lazily allocated bitmap of slots per page,
not a card table, so the scavenger visits exactly the recorded slots and
drops the dead ones. During marking the same flags route stores to a
Dijkstra insertion barrier that greys the value; objects allocated during
marking are allocated black. Every heap pointer folded into machine code
has a `RelocInfo` entry patched when the target moves. Allocation-site
pretenuring records a memento behind each literal and tenures sites that
survive. Pacing: the next limit from measured collector speed against
mutator speed; marking steps scheduled from the expected marking time
against the allocation rate. What V8 reverted: the non-moving young
generation, sticky mark bits, the dedicated map space, conservative stack
scanning for JS (unshipped after a decade). ([Trash talk][orinoco],
[concurrent marking][v8-cm], [pre-tenuring][v8-pretenure],
[Wingo 2025][v8-2025])

Taken: the parallel Scavenger with age mark, slot-set remembered sets,
the insertion barrier with black allocation, the barrier's elision rules
(numbers, initializing stores with no safepoint in between), memento
pretenuring, the shape of the pacer (measured speeds, not constants).
Not taken: relocation of code and compaction (the old space does not
move), the specific constants, and anything weak until ES6.

### Recent work (2023–2026)

- **Mark-region heaps.** Immix ([Blackburn and McKinley 2008][immix])
  divides the heap into 32 KB blocks of 128-byte lines, bump-allocates
  into runs of free lines, marks lines as it marks objects, and sweeps
  by reading line marks; it reclaims at line granularity without moving,
  and *opportunistically* evacuates the most fragmented blocks into holes
  when free space allows, so copying is bounded by the memory at hand.
  It is the heap under MMTk's production collectors (CRuby, Julia),
  Whippet's `nofl` space and LXR ([OOPSLA 2025][lxr]: 11% faster than
  G1 on a Google service by pairing concurrent marking with copying only
  in pauses). This design's old space is Immix with the evacuation
  decided from the sweep's measurements and executed in the final pause.
- **Pinning instead of conservatism everywhere.** CRuby and Julia
  (ISMM 2025) moved to MMTk's Immix with conservative *stack* roots whose
  targets are pinned in place while everything else moves; 70–99% of the
  heap stays movable ([Ruby][ruby-mmtk], [Julia][julia-mmtk]). Whippet's
  `nofl` space does the same for Guile with one metadata byte per 16-byte
  granule and a field-logging barrier measured at 1.05–1.5× over card
  marking ([Wingo 2024][nofl]). toi does not need the conservative half,
  but the pin bit is the fallback if a Rust frame ever has to hold an
  unrooted value.
- **Copying nurseries' failure mode.** A long live list built in one go
  survives its first minor collection whole and is copied again at the
  second ([Wingo 2025][nursery-fail]); the remedy is to promote in place
  when evacuation would copy most of a page. The nursery below has that.
- **Headers.** JEP 450/519 shrink Java's header to 8 bytes: 22% less heap,
  8% less CPU on SPECjbb ([lilliput]). JSC has been at 8 bytes since 2013.
- **Dynamic heapification** (PLDI 2024, [paper][heapify]): objects
  allocated on the stack optimistically and moved to the heap with
  reference fixing when the speculation fails. This is the published form
  of `object-model.md`'s externalization; its baseline (0.16% of objects
  stack-allocated by the JIT's escape analysis) is the number toi's
  bytecode-level virtual objects should beat.
- **Pacing.** CPython shipped an incremental collector in 3.14 and
  reverted it in 3.14.5 after production heaps grew up to 5× for a pause
  bound of 1.3 ms ([discussion][cpython-revert]). Go's Green Tea marker
  (default in 1.26) cut marking CPU 10–40% by scanning objects page by
  page rather than in discovery order ([Go blog][greentea]); the marker
  below queues work per block for the same reason.
- **Value encoding.** Float self-tagging (OOPSLA 2025, [paper][self-tag])
  reports 2.4–3.6× on float-heavy code against boxing. Out of scope here;
  the collector assumes nothing about numbers beyond "a cell is a pointer
  or an offset with the top bits clear".

## Design

### Threads

- **The mutator**: the one JS thread, as today. It allocates, runs the
  barrier, and executes every stop-the-world phase itself when it reaches
  a safepoint (JSC's "conn": the collector never touches frames, the
  handle stack or Rust-owned structures).
- **Collector threads**: a pool of `min(cores - 1, 4)` threads (the count
  is a tunable measured in step 6), idle when no cycle runs. They mark
  and sweep the old space, help copy during minor collections, and
  prepare memory (below). They read and write the cage, the mark bitmaps,
  the work queues and the block descriptors, and nothing else.

What is shared, and how:

| structure | written by | read by | synchronisation |
|---|---|---|---|
| cell fields | mutator | markers | relaxed atomic loads in the marker; every field is a `Value` or a cell pointer at every instant |
| mark bitmaps and line marks | markers, mutator (barrier, allocation during marking) | sweepers, mutator | atomic or-and-test per mark bit; a line mark is a byte store of 1, which any number of threads may repeat |
| recorded-slot bitmaps (remembered set and candidate slots) | mutator (barrier stub), markers (candidate slots) | mutator at the pauses | atomic or per bit; read only when the world is stopped |
| mark work queues | barrier (push), markers | markers | segmented queues: a thread owns a segment, publishes full ones to a shared list under a lock |
| remembered-set slot buffer | mutator | mutator (minor GC) | none: the minor collection is stop-the-world |
| block descriptors (state, holes, pins) | sweepers, mutator allocator, the start pause (candidates) | both | a state word per block: `free`, `recyclable`, `full`, `allocating`, `candidate`; sweepers hand recyclable blocks over through one queue; pins are set only by the mutator |
| `gc_requested`, phase | collector | mutator at safepoints | one atomic word |

The marker never reads a shape, a `Code`, a `Vm` field or an inline cache:
everything it needs is in the cell (below). That rule is what keeps the
concurrent part small; ckronos's collector reads 121 fields of its
runtime state and carries 53 numbered invariants to stay consistent.

### The heap

One reservation (the sandbox's cage, `docs/sandbox.md`; 64 GB of address
space costs nothing until touched), divided into:

- **Nursery**: two semi-spaces of `N` bytes each, contiguous, so *young*
  is one unsigned compare: `(cell - nursery_base) < 2N`. Allocation is a
  bump of `Heap::ptr` against `Heap::end`, as today; compiled code keeps
  bumping the same two words. A collector thread pre-faults and zeroes
  the to-space while the mutator fills the from-space, so allocation never
  takes a page fault on the mutator thread (the kernel's zeroing was most
  of allocation's cost in `future-work.md`, "Fewer page faults").
- **Old space**: 32 KB blocks (two Apple-silicon pages) of 256 lines of
  128 bytes, Immix's geometry. A cell is allocated at a 16-byte granule
  inside a *hole*, a run of free lines; cells up to a line are the common
  case (a 48-byte header and up to ten slots), cells up to 8 KB take a
  hole of enough lines, and the allocator keeps one block with large
  holes aside for them (Immix's overflow block) so they do not break up
  small holes. Per block, outside the block (trusted metadata, V8's
  lesson for the sandbox): a state word (`free`, `recyclable`, `full`,
  `allocating`, `candidate`), the line marks (one byte per line), the
  cell mark bitmap (one bit per granule, set at a cell's first granule),
  the pin bitmap (same geometry), a *version* that makes the marks
  logically clear without touching them (JSC), and the slot bitmap of
  recorded slots. Allocation is a bump: the mutator and each helper
  thread own a current hole (`ptr`, `end`), take the next hole of the
  block from its line marks, and the next recyclable block from the
  sweeper's queue. The nursery's bump and the old space's bump are the
  same two-word operation, so pretenured allocation in compiled code is
  the stencil it has today with another pair of words.
- **Large space**: cells over 8 KB (long strings, big element arrays,
  `ArrayBuffer` bytes), each its own range, never moved, one mark bit
  each. `ArrayBuffer` bytes always live here, even small, so a typed
  array's `elems`, an interior pointer into them, never needs fixing.
- **Metadata**: block descriptors, bitmaps, queues, the handle stack:
  Rust allocations outside the cage.

Offsets or addresses: cells are raw 48-bit addresses today and 32-bit
offsets in the sandbox. Every test in this design is of the form
`addr - base < size`, which reads the same either way, so the collector is
written over a `Cage` with `base` and the tests stay when the encoding
changes.

### Cells

Every allocation is a cell of one of these kinds, and the collector finds
a cell's size and pointer fields from the cell alone:

| kind | header | size from | pointer fields |
|---|---|---|---|
| object (`Class::Plain`, `Function`, `Native`, `Date`, `RegExp`, …) | `Object`, 48 bytes | `inline_cap` byte, plus `kind` as upvalue count for functions | inline slots, `props`, `elems`, upvalues; `code` only for `TypedArray` (its buffer) and cons strings (the second half) |
| array | same | same | same, `elems` |
| string | same, units follow | `elems_len` | none, or `props`/`code` for a cons |
| typed array / buffer view | same | fixed | `code` (the buffer); `elems` recomputed from it |
| property array (`props`) | 8 bytes: capacity | its header | all |
| element array (`elems`) | 8 bytes: capacity | its header | all |
| upvalue | none: its block's kind says "upvalues" | fixed, 16 bytes | `closed`, or `at` into a frame (or at its own `closed`) |
| buffer bytes | 8 bytes: length | its header | none |

Two changes to today's layout, both for the concurrent marker:

- **The header says how big the object is.** `inline_cap` goes in the
  header's spare byte at allocation and `kind` doubles as the upvalue
  count for functions, so the marker never reads a shape (a Rust `Box`
  the mutator mutates) nor a `Code`. A function with more than 255
  upvalues gets them in a separate array cell.
- **Property and element arrays get a capacity word.** Today their
  capacity lives in the owner (`props_cap`, `elems_cap`), and growth
  writes a new pointer and a new capacity in two stores; a marker reading
  the old pointer with the new capacity would run off the end of the old
  array into unrelated cells. With the capacity in the array, the marker
  reads the pointer, then the length from what it points to, and both
  belong to the same array whether old or new. The old array is garbage
  it may still scan, which is harmless: its slots hold the values they
  held, all valid. This is V8's `FixedArray` and the reason JSC's
  butterfly carries its own lengths. The owner's `props_cap` and
  `elems_cap` stay for the interpreter's and the stencils' fast paths.

A property or element array is referenced only by its owner (the
`arguments` object copies, `string_units` flattens into a fresh units
cell), which the verifier checks; the owner traces it and copies it along
when it is copied.

The object header keeps its 48 bytes in this design. The collector needs
from it `shape`, which a copy, in the nursery or an evacuation,
overwrites with the forwarding pointer plus a tag bit (shapes are
8-aligned Rust boxes, so bit 0 is free; the parallel copy installs it
with a compare-and-swap), `class`, `inline_cap` and `kind`. No mark bit:
marks live in the block bitmaps. No age bit: age is position relative to
the nursery's age mark, as in V8. Two cells point into themselves, a
string's `elems` at its inline units and a closed upvalue's `at` at its
own `closed`; a copy re-aims them, and nothing else is an interior
pointer, since buffers never move.

### Roots

The precise root set, taken by the mutator at a pause:

1. **VM frames**, walked from the current frame through `regs[-CALLER]`:
   every register `r0..nregs` of every frame, and the header's `FUNC`
   slot (the running closure). Every register holds a valid `Value` at a
   safepoint: the interpreter writes nothing else, and compiled code
   writes back only registers live at the exit, boxed. Dead registers are
   scanned too; what they hold is a valid value that keeps something
   alive until the frame returns, as in V8's and JSC's interpreters. The
   alternative, liveness maps per `Code` and pc, is a later refinement
   the JIT's `exit_live` already computes.
2. **Compiled frames at a runtime call.** A region's call wrapper pushes
   the live slot registers on the native stack. Each runtime call site
   gets a *safepoint map*: the frame, how many GPR slots were pushed and
   a bit per pushed slot for "boxed" (unboxed `Int` and `Fpr` homes hold
   no pointers), and whether `acc` is boxed there. The map is a side table
   keyed by the wrapper's return address, built when the region is
   emitted from the same `Home` assignment the exits use. Direct calls
   (`arch::direct_call`) push the same way and get the same map.
3. **The handle stack**: `Vm::handles`, a `Vec<Value>` with scoped depth,
   for Rust code that holds a value across a call into JS (below).
4. **Open upvalues**: `Vm::open_upvalues`, each an upvalue cell.
5. **Named VM roots**: the global object, every builtin prototype,
   `missing_holder`, `single_chars`, `bind_helper`, `array_fn` and the
   other cached functions, `error`'s value, `pending_args` (made to point
   into the callee frame rather than a Rust slice).
6. **Old-only references**, visited by a major cycle but never by a
   minor one, because they may only refer to old cells: `Code::constants`
   and the string constant table (allocated old), inline caches' `holder`
   and `new_shape`'s prototypes, `Shape::proto`, `CallSite::closure`,
   `CtorSite::proto`, folded constants and prototype slot addresses in
   compiled code (`relied_slots`), `Vm::regexps`' objects. The rule that
   keeps them old is below; the barrier that keeps them marked during a
   cycle is the same as for any store.

The interning table `Vm::strings` holds constants, which are old and live
as long as their `Code`; runtime strings are never interned
(`string_constant` is called from the compiler and from property-name
conversion only), so the table is a plain root, not Lua's weak set.

### Safepoints, and why Rust code needs no handles

**A collection runs only at a safepoint, never inside an allocation.**
When the nursery is full, the allocator takes the next chunk of to-space
headroom (or, if none is left, a fresh chunk) and sets `Vm::gc_requested`;
the collection happens at the next safepoint. The collector threads
request their two pauses through the same word. Safepoints are:

- the interpreter's loop back edge (next to the budget countdown that
  tiers up), and a call before the callee's frame is pushed;
- a runtime call from compiled code, in the wrapper, with the safepoint
  map; the allocation slow path is one;
- `call_function` and `run_script_at`, before entering the new frame.

Compiled loops get no poll of their own: a loop that allocates reaches the
allocation slow path within one nursery of allocation, and a loop that
does not allocate cannot need a collection. A pause the collector asks for
waits until the mutator next allocates; while it waits, nothing grows.

So a runtime function or builtin that allocates ten times in a row holds
its `Value`s in Rust locals safely: nothing moves under it. The only Rust
code that must root is code that **calls back into JS** and keeps a value
across the call: `each` and the other callback builtins (`sort`, `map`,
`filter`, `reduce`, `replace` with a function), accessor calls in
`Vm::get`/`Vm::set`, `to_primitive` and `to_string` on objects. There are
37 `call_function` sites and the accessor path to audit. The API is a
scope: `vm.rooted(v)` pushes on the handle stack and returns an index;
the scope's drop truncates; natives read `vm.handles[i]` after the call.
This is Lua's rule for its C API (values live on the Lua stack) and what
`native_top` already does for frames.

`TOI_GC_STRESS=1` collects at every safepoint, flips the nursery every
time, and fills the vacated semi-space with `0xdd`, so a missed root
crashes on first use rather than corrupting silently. The differential
test suite runs under it, and under `TOI_GC_STW=1` (every phase on the
mutator thread, the debugging configuration), as two more of its
configurations; ckronos tested only its stop-the-world mode and shipped
the concurrent one.

### The nursery collection

Cheney copying, with V8's age mark, stop-the-world, parallel:

1. Flip. From-space is the nursery that was being allocated into; the
   age mark is where allocation stood after the previous collection.
2. The mutator scans the roots (1–5 above) and the remembered set's
   slots, posting each slot that holds a young value as a work item; the
   collector threads and the mutator consume items: for an object above
   the age mark, allocate in to-space from a per-thread allocation buffer
   and compare-and-swap the forwarding word; the thread that wins copies,
   the others read the forwarding pointer. For an object **below the age
   mark**, which survived once already, *promote*: allocate in the old
   space from a per-thread block, copy, and scan the fields for young
   pointers, which go in the remembered set. Copied objects are scanned
   from a per-thread worklist with stealing.
3. Set the age mark at the new allocation top; the from-space becomes
   the next to-space, and a collector thread starts zeroing it.

If copying would move more than most of a from-space page (the fraction
is measured, not preset: the point where copying costs more than the
fragmentation it saves), the page is promoted whole by re-threading it
into the old space as a block of mixed sizes that is swept but never
allocated into again (V8's page promotion; the answer to the long-live-list
failure mode). This needs object-start bits for that page only, computed
while copying.

A minor collection during a major cycle stops the markers too. Promoted
objects are allocated marked and pushed grey, so the marker scans them
once initialized; the mark queues and the remembered set hold only old
addresses, so copying changes nothing they contain.

**Pretenuring.** Allocation sites that survive are made to allocate
directly in the old space: a `CreateObject`, `CreateArray` or
`construct_direct` site records a memento after a sample of its objects
(one word, the site), the collection counts mementos behind survivors,
and a site whose sampled survival stays high over enough samples switches
to old allocation; the JIT's inline allocation for that site is dropped
and recompiles against the old-space allocator. The sample rate, the
number of samples and the survival cut-off are tunables of step 6; the
decisions reset when a major cycle frees most of the old space. This is
what replaces "allocate external directly" from `object-model.md`.

### The write barrier

Needed on every store of a value into a heap cell: `SetNamed`,
`SetKeyed`, element stores, `props`/`elems` installation, upvalue close,
`store_field`, every builtin's store, and every creation of an old-only
reference (an inline cache filled, a constant folded, a shape's prototype
set). Two conditions share it: the *generational* one, the host is old and
the value is a young cell, and the *marking* one, a cycle is marking and
the value is an old cell not yet marked.

Compiled code, x86-64, after the store of `v` into `[host + off]`:

```text
mov rax, v ; sub rax, [young_base]     ; is v a cell we care about?
cmp rax, [young_span] ; jae done       ;   numbers (≥ 2^49) and immediates (< 16) never are
cmp host, [young_end] ; jb done        ; host young: never a barrier
lea rax, [host + off] ; call slow      ; out-of-line stub, not a runtime call
done:
```

`young_span` is `2N` outside a marking cycle and `2^48` during one, so
during marking every *cell* store into an old host takes the stub and
every number store still takes three instructions (JSC's threshold trick,
applied to a range). Storing an old value into a young host needs no
marking barrier either: the nursery is one grey object, scanned in full at
the final pause. The stub does both jobs: if the value is young, append
the slot to the remembered-set buffer (flushed into the block's slot
bitmap when full); if marking and the value is old and its mark bit is
clear, set it and push the cell on the mutator's mark segment. No fence:
one mutator, and the markers tolerate seeing a grey cell late because the
final pause drains everything.

The remembered set is V8's: precise slots, so the minor collection visits
exactly what was recorded and drops what died. A card table would be
branchless but makes the minor collection rescan whole cards and find
object starts; JSC's object-granular barrier rescans whole objects, wrong
for a 10,000-element old array taking one store. The stub is where to
swap either in if measurement says so.

The barrier is elided where the JIT knows the store can create neither
edge:

- the value's type is `Int` or `Num` (field types: DeltaBlue's hot stores,
  `planner.currentMark = …`, are numbers, and the type is static);
- the host is an object allocated in this region with no safepoint since
  (a `CreateObject` or scalar replacement being materialized): the
  Maglev CVEs of 2024 came from a collection between two folded
  allocations, so the rule is tied to the safepoint list above;
- the host is a folded constant, old by the old-only rule, and the value
  is a folded constant too.

The interpreter and the runtime call one `write_barrier` function with the
same condition.

### The old-only rule, and pins

**A pointer stored in compiled code, an inline cache, a shape, a call site
or any Rust-side cache refers to an old cell, and that cell is pinned.**
Then nothing outside the heap is ever patched: a minor collection does
not look at those places at all, and an evacuation leaves pinned cells
where they are. Pinning is a bit in the block's pin bitmap, set by the
same call that stores the pointer, never cleared (a `Code` is never freed
today; when it is, its pins go with it). A few thousand cells at most are
pinned in a run: prototypes, constant objects on globals, closures at
direct call sites. Enforcement at each site:

- *Folded constants and prototype slot addresses* (`fold_constant_loads`,
  `rely_on_slot`): if the object is young, do not fold; emit the load and
  check instead. Compiled code is compiled after warm-up, so the case is
  rare, and the object is old two minor collections later.
- *Call and constructor sites* (`CallSite::closure`, `CtorSite::proto`):
  a young closure or prototype leaves the site unbound (calls go through
  the runtime) until it is old; `toi_rt_call` rebinds it, as it does
  today after exits.
- *Inline caches*: a prototype `holder` that is young is not cached; the
  miss path handles the load until it is old.
- *Shapes*: `Shape::proto` is the one Rust-side pointer that cannot wait,
  since a shape is made the moment `new F` runs. Shapes whose prototype
  is young go on a list the minor collection walks to update `proto`; a
  shape leaves the list when its prototype is promoted. The root-shape map
  `Vm::roots`, keyed today by the prototype's address, is keyed by the
  prototype's *unique shape* instead, which is a stable Rust pointer.
- *Constants*: `Code::constants`, interned strings, `single_chars`, the
  builtins and their prototypes are allocated in the old space directly
  (`Heap::alloc_old`), as V8 does for internalized strings, into blocks
  marked `pinned` whole, which the evacuation never selects.

Each of these is a store of a cell pointer into something the marker does
not read, so each runs the barrier's marking half when it happens during a
cycle, sets the pin bit, and a major cycle's pauses treat them all as
strong roots while their owner (`Code`, `JitFunction`, `Shape`) lives.
The named VM roots (the global object, the builtin prototypes) need no
pin: the pause updates them like any root. JSC holds structures
weakly from code and jettisons code when they die; toi's `Code`s are never
freed today, so weak references buy nothing yet. When they are, the
dependency lists the JIT keeps (`field_deps`, `jitted`) are where the
jettison goes.

### The major cycle

Concurrent, with two pauses, on JSC's and V8's pattern:

1. **Start pause** (mutator, at a safepoint): bump the mark version, so
   every block's marks are logically clear; if the last sweep found the
   old space fragmented (below), choose the *evacuation candidates*: the
   blocks with the fewest live lines, without pinned blocks, up to the
   cycle's copy budget, and flag them; scan the roots 1–6 above and the
   remembered set's hosts into the shared mark queues; set `young_span`
   to `2^48`; wake the markers. Microseconds to a millisecond: the roots
   are frames and a few tables.
2. **Concurrent mark** (collector threads): drain the queues with
   stealing; a popped cell is traced by its kind, each old referent's
   mark bit set with an atomic test-and-set and its lines marked, newly
   set ones pushed. A slot whose referent is in a candidate block (and
   not pinned) is *recorded* in the host block's slot bitmap, the same
   structure the remembered set uses; the barrier stub records such
   stores made meanwhile. Work is queued per block where it can be
   (Green Tea's observation: cells of one block are scanned together,
   which keeps the scan in cache), with the queue of blocks replacing the
   queue of cells once the first wave of discovery is done. Young
   referents are ignored: the nursery is scanned at the end. Old cells
   allocated meanwhile (promotions, pretenured sites) are allocated
   marked, lines included, and pushed grey. The markers stop when the
   queues stay empty for a short while, then request the final pause.
3. **Final pause** (mutator, with the collector threads helping): a minor
   collection first, which empties the nursery of garbage and makes its
   survivors the "grey nursery"; rescan roots 1–6; trace the nursery's
   survivors to mark their old referents; drain the queues to empty
   (bounded by what the barrier pushed since the markers stopped, which
   the markers kept small by not stopping until the queues were quiet).
   Then, if there are candidates, **evacuate**: each thread walks the
   marked, unpinned cells of its candidate blocks, copies each into its
   current hole (a recyclable block from the previous sweep, or a fresh
   one), marks the copy and its lines, leaves the forwarding pointer in
   the old cell, and clears the old cell's mark and lines; then every
   recorded slot, every root 1–5 and every field of a nursery survivor
   that points at a forwarded cell is rewritten, and a promoted or moved
   cell's young pointers are re-recorded in the remembered set. Reset
   `young_span`; wake the sweepers. Expected in the low milliseconds
   without evacuation, plus the copy budget at the measured parallel
   copy rate with it; the budget is set from that rate and the pause
   target.
4. **Concurrent sweep** (collector threads): for each block, read the
   line marks: no live line, the block is free and its memory given back
   with `madvise`; all lines live, `full`; otherwise `recyclable`, its
   holes listed and the block queued for the allocator. No cell is
   touched. The sweep also totals the live lines against the live bytes
   per block, which is the fragmentation measure the next start pause
   reads. Large cells without a mark are unmapped. If the allocator's
   queue is empty it sweeps one block itself (JSC's sweep-to-allocate,
   which keeps the mutator from waiting).
5. **Prepare**: pre-zero and pre-fault the blocks the allocator will
   take next, and the nursery's to-space.

Correctness of concurrent marking rests on four facts: nothing moves
while the markers run; every field holds a valid value at every instant
(the mutator writes words, and a word is a `Value` or a cell pointer); the
capacity of an array lives in the array; and every store of an old cell
pointer into an old host, or into anything the marker does not read, runs
the barrier. Correctness of evacuation rests on two more: every slot that
points into a candidate at the final pause is either a root, a field of a
nursery survivor, or recorded (the marker saw it, or the barrier did,
because it was written into an old host during the cycle); and pinned
cells never move. The verifier (`TOI_GC_VERIFY`) checks at the final
pause that no live old cell is unmarked, by a full stop-the-world mark it
compares against, and after an evacuation that no live word points at a
forwarded cell.

### Pacing, for performance

The collector's job is to maximise mutator throughput under a memory
cap, with the other cores as the resource it spends. No preset numbers;
the policy measures and adjusts.

- **Nursery size.** The cost of a minor collection is the roots plus the
  survivors it copies; its benefit is the garbage it frees for nothing.
  A larger nursery gives objects more time to die, so it copies fewer
  bytes per byte allocated, until the survival curve flattens; a smaller
  one keeps allocation in cache (write-allocate misses are the cost of
  bumping through fresh memory). The policy climbs: it records copied
  bytes per allocated byte and mutator time per allocated byte at the
  current size, tries double and half every few collections, and keeps
  what measured best, between 256 KB and the memory cap's share. The
  starting point is the last-level cache size. DeltaBlue's 4,400
  deterministic iterations allocated 832 MB, about 190 KB each; Splay's
  1,400 allocated 4.9 GB, 3.5 MB each; those are the two profiles the
  climb must handle.
- **Old-space headroom.** With concurrent marking the cost of a cycle to
  the mutator is the two pauses, the barrier's slow path and the memory
  bandwidth the markers take; the cost of headroom is memory. The policy
  sets the next cycle's trigger so that the collector's share of CPU
  (marking and sweeping time across threads, divided by elapsed mutator
  time) stays under a target, growing the headroom when the share is
  above it and shrinking when below, clamped by the cap. The cap is the
  only fixed number: `TOI_HEAP_LIMIT`, else a fraction of physical
  memory. Under the cap the mutator stops and waits for the sweep, which
  is what every collector does there.
- **When marking starts.** Marking must finish before the headroom runs
  out: start when `headroom < allocation_rate × predicted_mark_time ×
  slack`, with the mark time predicted from live bytes at the last cycle
  over the measured marking rate, and the allocation rate measured over
  the last few minor collections (V8's and Go's pacers both work this
  way). If the mutator outruns the markers, more collector threads join;
  if the headroom is gone before marking ends, the mutator marks too
  (JSC's scheduler stops the mutator; this does the same with the mutator
  doing useful work).
- **When to evacuate.** Fragmentation is the sweep's count of live
  lines against live bytes: the memory held by lines that are partly
  dead. Without evacuation it is bounded by a line per live cell; the
  policy evacuates when that memory exceeds what the headroom controller
  would otherwise have to grant, that is, when compacting is cheaper than
  the memory it would save at the current target. The copy budget per
  cycle comes from the pause target and the measured parallel copy rate,
  and candidates are taken sparsest first, so each copied byte frees the
  most. Fragmentation that never crosses the line costs nothing: most
  runs will never evacuate.
- **Thread counts.** The number of markers and copy helpers is chosen
  per cycle from the work left and the cores idle; measured in step 6
  rather than fixed.

`TOI_GC_STATS` prints, per collection: kind, pause, bytes allocated since
the last, bytes copied and promoted, marking and sweeping time per thread,
and the mutator's share of wall time; `scripts/gc-tune.sh` sweeps the
knobs on Octane and `bench/micro` to set the defaults the policy starts
from.

### Weak references, finalization

None in ES5. `WeakMap` and `WeakRef` arrive with ES6 and need ephemeron
processing in the final pause (V8's fixpoint, then a linear pass; Lua's
`convergeephemerons`). Rust-side resources reachable from cells, `Regex`
boxes and buffer memory, are freed by the sweeper when the cell dies;
nothing else needs a finalizer.

### Interaction with the JIT

- **Inline allocation** keeps its two-word bump against `Heap::ptr`/`end`
  (`CtorSite::heap`). The slow path is a runtime call, so a safepoint; the
  stencil re-reads `ptr` after it.
- **Stores** get the barrier above as a stencil tail, elided by type.
  `array_push` and element stores barrier the element array cell.
- **Safepoint maps** at runtime and direct call sites, from the `Home`
  assignment; the exit stubs are unchanged.
- **Old-only** checks at lowering, the pin bit on every pointer lowering
  embeds, and the barrier's marking half when that happens during a
  cycle.
- **Code epoch**: nothing new; the collector never invalidates code,
  because nothing it references moves.
- **Pretenured allocation** in compiled code bumps the old-space hole's
  two words exactly as the nursery's; the slow path takes the next hole.
- **Scalar replacement** materializes into the nursery like any
  allocation; between the allocation and the last initializing store there
  is no safepoint, so no barrier.

### What `object-model.md` keeps

Virtual objects at the bytecode level (A), compact layouts (B), the
measurement plan for DeltaBlue's allocation sites, and the barrier-free
argument for hot stores (now: numeric stores need no barrier, and stores
into young objects need none). The externalization barrier, the external
bit, the transitive walk and the two-heap invariant go.

## Plan

Each step lands with its tests and a measurement against the previous
one on `scripts/octane.sh` (time and peak RSS) and `bench/micro`. The
concurrent structures (atomic mark bits, segmented queues, the block
state word, compare-and-swap forwarding) are built in from step 1; the
stop-the-world configuration is the debugging mode, not the first design.

1. **Heap layout, no collection.** The cage, nursery semi-spaces, old
   blocks with descriptors and line marks, hole allocation, large space,
   `alloc_old`; constants and builtins allocated old into pinned blocks;
   `inline_cap` in the header; property and element arrays with their
   capacity word; upvalues in their blocks. Everything still leaks. Checks: byte-identical results, page faults
   and time on the allocation microbenchmarks not worse.
2. **Roots and verification.** The frame walk, safepoint maps for
   runtime and direct calls, the handle stack and the audit of the 37
   callback sites and the accessor path, `gc_requested` at safepoints.
   `TOI_GC_VERIFY=1` traces the whole heap from the roots at every
   safepoint and asserts every root word and every traced field is a
   valid value pointing at a live cell, before anything is freed. This
   step finds the missing roots; it frees nothing.
3. **The nursery collection, single-threaded.** Cheney copy, age mark,
   promotion, the barrier in the interpreter and runtime, then in the
   stencils, the old-only rule at lowering, `TOI_GC_STRESS`. Target:
   Splay and DeltaBlue at a fraction of today's RSS with less than 10%
   more time; a mutation test that removes one barrier call fails under
   stress.
4. **The major cycle, concurrent.** The collector threads, the two
   pauses, concurrent marking with line marks and the barrier's marking
   half, concurrent line sweeping, the verifier's stop-the-world
   comparison, `TOI_GC_STW`. Target: pdfjs and TypeScript peak RSS near
   their live size times the headroom, EarleyBoyer and RegExp with no
   paging, and the mutator's share of wall time above 90% on all of
   Octane.
5. **Parallel copying and memory preparation.** Helper threads in the
   minor pause, pre-zeroed to-space and blocks. Target: minor pauses on
   Splay down by the measured parallel share.
6. **Evacuation.** Pins, candidate selection from the sweep's
   fragmentation measure, slot recording in the marker and the barrier
   stub, the copy and rewrite in the final pause, the verifier's
   forwarded-pointer check. Test: a program that allocates a large
   population, keeps one object in a thousand and then allocates objects
   of another size, with `TOI_GC_STATS` showing the old space's blocks
   returning to the live size; the same program must also stay bounded
   with evacuation disabled, which is the line-granularity claim.
7. **Pacing and tuning.** The nursery climb, the headroom controller,
   the marking start predictor, the evacuation trigger and budget, thread
   counts, pretenuring's sample and cut-off, measured with
   `scripts/gc-tune.sh`.
8. **Later.** The header diet (shape as a 32-bit ID and class, flags and
   lengths in one word: 16 bytes, with `props` and `elems` behind one
   pointer as JSC's butterfly), which also puts most objects within one
   line; shapes as collectable cells with weak references from code,
   which lets pins expire; ephemerons with ES6.

## Risks

- **A missed root or barrier is silent.** Step 2's verifier and step 3's
  stress mode are the defence, and they run in CI across the test suite's
  configurations, concurrent and stop-the-world both. The audit of Rust
  code that calls back into JS is the manual part; the list is short
  because reentry goes through `call_function` and the accessor path only.
- **Concurrency bugs are the expensive kind.** The design keeps the
  shared state to five structures (the table above) and the marker's
  reads to the cage alone, and the verifier compares every concurrent
  mark against a stop-the-world one under `TOI_GC_VERIFY`. Termination
  uses the plain counting protocol (a shared count of busy markers,
  queues checked twice); ckronos's livelock came from a cleverer one.
- **Promotion churn.** A graph live for one long call (DeltaBlue's solver
  state, Splay's tree) is promoted and dies in the old space. Pretenuring
  and the nursery climb reduce it; the concurrent marker makes what is
  left cost another core rather than the mutator. Measured in step 6
  against the internal/external alternative's claim.
- **Memory bandwidth.** Markers share the memory system with the
  mutator; on a laptop with a 1.5 GB live heap they can slow it. The
  headroom controller's CPU-share target is also the knob for this, and
  thread counts are measured, not assumed.
- **Pins that never expire.** A pinned cell holds its line for the life
  of the `Code` that pinned it, and `Code`s are not freed today. The
  count is small and the memory is a line per pin, but a block full of
  pins is never a candidate, so a long-running embedding that compiles
  without bound accumulates unevacuable blocks. Shapes and code as
  collectable cells (step 8) is the fix; until then `TOI_GC_STATS`
  reports pinned lines so the growth is visible.
- **A missed recorded slot leaves a dangling pointer after an
  evacuation.** The recording happens in exactly two places, the
  marker's trace and the barrier stub, and the verifier's
  forwarded-pointer check runs under `TOI_GC_VERIFY` after every
  evacuation; `TOI_GC_STRESS` makes every cycle evacuate every eligible
  block, so a missed slot shows up in the test suite rather than in a
  long run.
- **The sandbox's offsets.** The design is written over `base`-relative
  tests so the switch is mechanical; `Heap::alloc_old` and the block
  descriptors are the two places that compute addresses from offsets.

## Sources

[v8-2025]: https://wingolog.org/archives/2025/11/13/the-last-couple-years-in-v8s-garbage-collector
[v8-cm]: https://v8.dev/blog/concurrent-marking
[riptide]: https://webkit.org/blog/7122/introducing-riptide-webkits-retreating-wavefront-concurrent-garbage-collector/
[jsc-2022]: https://webkit.org/blog/12967/understanding-gc-in-jsc-from-scratch/
[lgc]: https://github.com/lua/lua/blob/master/lgc.c
[lua-ws18]: https://www.lua.org/wshop18/Ierusalimschy.pdf
[orinoco]: https://v8.dev/blog/trash-talk
[v8-pretenure]: https://wingolog.org/archives/2026/01/05/pre-tenuring-in-v8
[lilliput]: https://openjdk.org/jeps/519
[ruby-mmtk]: https://railsatscale.com/2025-09-16-reworking-memory-management-in-cruby/paper.pdf
[julia-mmtk]: https://www.steveblackburn.org/pubs/papers/julia-ismm-2025.pdf
[nofl]: https://wingolog.org/archives/2024/10/03/preliminary-notes-on-a-nofl-field-logging-barrier
[nursery-fail]: https://wingolog.org/archives/2025/01/13/an-annoying-failure-mode-of-copying-nurseries
[heapify]: https://doi.org/10.1145/3656389
[cpython-revert]: https://discuss.python.org/t/reverting-the-incremental-gc-in-python-3-14-and-3-15/107014
[greentea]: https://go.dev/blog/greenteagc
[immix]: https://www.steveblackburn.org/pubs/papers/immix-pldi-2008.pdf
[lxr]: https://www.steveblackburn.org/pubs/papers/prod-oopsla-2025.pdf
[self-tag]: https://arxiv.org/abs/2411.16544

Also read: V8's `src/heap` (`scavenger.cc`, `heap-write-barrier-inl.h`,
`slot-set.h`, `heap-controller.cc`, `concurrent-marking.cc`,
`reloc-info.h`, `pretenuring-handler.cc`), JSC's `heap/MarkedBlock.h`,
`FreeList.h`, `CellState.h`, `ConservativeRoots.cpp`, `Collector.cpp`,
`gc_thread.rs` and `concurrent.rs` of ckronos as a worked example of a
concurrent marker over a non-moving heap and of what its 27k lines paid
for; Lua 5.4.7 and 5.5.1 `lgc.c`; Whippet's FOSDEM 2025 slides; "The
evolution of Lua, continued" §4.4.
