# The garbage collector

Status: 2026-10-10. Steps 1 to 6 of the plan below are implemented in
`src/heap.rs` and `src/gc.rs` (step 7 is later work): minor collections,
and major ones marked and swept by a second thread between two short
pauses (`CLOISON_GC_STW` keeps the stop-the-world full collection).
This revision replaces the first one (a parallel copying
nursery with precise roots), whose two premises implementation showed
false: see "Why not a copying nursery". Supersedes the collector parts of
`docs/object-model.md` (its escape analysis and compact layouts stand).

## Decision

A **non-moving generational mark-region collector with conservative
native roots, concurrent on the old generation**:

- one reservation of address space in 32 KiB blocks; cells up to 8 KiB
  are bump-allocated through *holes* (runs of free 16-byte granules) and
  never cross a block, larger cells get blocks of their own;
- one metadata byte per granule beside the heap: allocation writes
  "young" at a cell's first granule, marking replaces it with the mark
  color and writes "end" at its last granule; so liveness, age, cell
  starts and free space are all read from the side table (Whippet's
  `nofl` space, [Wingo 2024][nofl]);
- generations by sticky marks: a minor collection marks the young cells
  reachable from the roots and from dirty cards, survivors become old
  where they are, and the rest of the nursery's memory is holes again;
- roots: the VM stack and the native stack scanned conservatively, every
  word checked against the metadata, plus the VM's own references;
- one write barrier, a card mark of the stored-to address when the store
  makes an old cell refer to a young one (two metadata loads decide it),
  which is the minor collection's remembered set and, widened to every
  store of an unmarked cell while marking runs, concurrent marking's
  record of mutated cells (the HotSpot CMS arrangement);
- major collections marked by a collector thread while the mutator runs,
  with two short pauses, and swept concurrently from the metadata;
- pacing from measurement.

Collections run only at safepoints: allocation never collects, so Rust
runtime code may hold values in locals across allocations; the native
stack scan covers them, and the few natives that keep values in Rust
heap containers across a call into JS hold them in a handle scope.

## Why not a copying nursery

The first revision of this document chose V8's design: a semi-space
nursery copied by a parallel Scavenger, precise roots read from VM frames
and safepoint maps, and an old space that moved only in pauses. Two of
its premises do not hold in cloison, which implementation showed:

1. **Roots in compiled frames are not precise.** Stencils are machine
   code that LLVM compiled; across a runtime call, LLVM keeps live values
   in callee-saved registers (`rbx`, `rbp` on x86-64), and on arm64 the
   GPR slot registers x24–x28 are callee-saved under the C ABI, so the
   runtime-call wrapper does not push them: they pass through Rust frames
   that save them wherever their prologues put them. A collection reached
   from a runtime call can find those words only by scanning the native
   stack, and cannot tell a boxed value from an integer that looks like
   one, so it cannot rewrite them: what they point to must not move.
2. **Compiled code embeds heap addresses everywhere.** Inlining guards
   compare the callee register with a function object, prototype loads
   read an absolute slot address, the global object and folded constants
   are immediates, inlined frames read upvalue cells by address, call and
   constructor sites cache closures and prototypes. Under a moving
   nursery none of these may be young, so a function allocated since the
   last collection could not be inlined; in a short run that is every
   function, and the microbenchmarks are short runs.

A third cost tipped it: Rust runtime code holds values, raw object
pointers and slices of string units across allocations everywhere
(`string_units` returns a slice into the heap), and a moving collector
needs each one in a handle.

With cells that never move, compiled code keeps embedding what it likes
(the collector keeps those cells alive), conservative roots cost a
metadata check per word, and Rust code needs no handles except across a
call into JS. What is given up is compaction of the nursery: survivors
stay where they were allocated, interleaved with the holes that dead
cells leave, and allocation bumps through holes rather than one free
range. V8 measured its non-moving young generation as a loss against
the Scavenger; Whippet measured `nofl` with sticky marks as competitive
for Guile, whose roots are conservative like cloison's. The plan measures it
at each step.

## Numbers the design is for

Octane's deterministic runs on cloison before the collector (2026-10-10,
Apple silicon, `target/release/cloison`; with no collector the heap's size is the
allocation volume; peak RSS undercounts it on macOS, which compresses
memory that is never touched again):

| benchmark | allocated | time | rate |
|---|---:|---:|---:|
| EarleyBoyer | 17.3 GB | 28.7 s | 600 MB/s |
| RayTrace | 13.6 GB | 10.8 s | 1.3 GB/s |
| Splay | 4.9 GB | 2.4 s | 2.0 GB/s |
| RegExp | 2.5 GB | 18.6 s | 135 MB/s |
| DeltaBlue | 1.8 GB | 1.2 s | 1.5 GB/s |
| pdfjs | 1.4 GB | 2.4 s | 590 MB/s |
| Crypto | 769 MB | 5.6 s | 140 MB/s |
| TypeScript | 393 MB | 4.1 s | 95 MB/s |
| Box2D | 183 MB | 1.7 s | |
| GBEmu | 166 MB | 1.3 s | |
| code-load | 123 MB | 2.7 s | |
| Richards | 40 MB | 1.6 s | |
| NavierStokes | 10 MB | 7.1 s | |

(The volumes are the heap's own count; the first revision of this table
took peak RSS for them, which macOS's memory compression held to a
quarter of EarleyBoyer's and a tenth of RayTrace's.)

The heap of step 1 rounds cells to 16 bytes and gives arrays a header
word: DeltaBlue allocates 12% more, Splay 9%, code-load 28%.

Two shapes of workload: DeltaBlue, Richards, RayTrace and Splay allocate
small objects at hundreds of MB/s with most dying young (Splay's tree
nodes are the exception that generational collectors are tested on), while
pdfjs, TypeScript and code-load build large structures that live until the
end. The minor collection pays for the first group; the old generation's
headroom policy and the concurrent marker decide the second.

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

Taken: non-moving cells with side mark state, generations by sticky
marks, conservative scanning of the machine stack (for the same reason
JSC has it: compiled code and C++, here Rust, hold raw pointers), bump
allocation over free intervals (JSC's intervals are Immix's holes),
concurrent lazy sweeping, the two-pause concurrent cycle, code holding
heap constants, the collector pacing against mutator progress. Not
taken: size-classed blocks (a block of one size class can be held by
one live cell), the object-granular barrier (a card mark is cheaper on
the store, and the minor collection pays instead), IsoSubspaces.

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

Taken: the barrier's elision rules (numbers, initializing stores with no
safepoint in between), black allocation during marking, the shape of the
pacer (measured speeds, not constants). Not taken, after trying to fit
it (see "Why not a copying nursery"): the Scavenger, which needs every
pointer to a young object to be found and rewritten, and `RelocInfo`'s
patching of code. V8 found its non-moving young generation slower than
the Scavenger; V8 can move because it controls every frame layout its
compilers produce, which a copy-and-patch JIT built from LLVM's output
does not.

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
  marking ([Wingo 2024][nofl]). cloison's heap is `nofl`'s: the granule
  metadata byte, conservative roots checked against it, holes found from
  it, generations by sticky marks, with a card-marking barrier.
- **Copying nurseries' failure mode.** A long live list built in one go
  survives its first minor collection whole and is copied again at the
  second ([Wingo 2025][nursery-fail]); the remedy is to promote in place
  when evacuation would copy most of a page. A non-moving young
  generation promotes everything in place.
- **Headers.** JEP 450/519 shrink Java's header to 8 bytes: 22% less heap,
  8% less CPU on SPECjbb ([lilliput]). JSC has been at 8 bytes since 2013.
- **Dynamic heapification** (PLDI 2024, [paper][heapify]): objects
  allocated on the stack optimistically and moved to the heap with
  reference fixing when the speculation fails. This is the published form
  of `object-model.md`'s externalization; its baseline (0.16% of objects
  stack-allocated by the JIT's escape analysis) is the number cloison's
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

- **The mutator**: the one JS thread. It allocates, runs the barrier, and
  runs every stop-the-world phase itself at a safepoint (JSC's "conn":
  only the mutator reads frames, the native stack and Rust structures).
- **A collector thread** (later a small pool) marks and sweeps the old
  generation while the mutator runs. It reads cells and writes the
  metadata, the mark queues and the block lists, nothing else: it never
  reads a shape, a `Code`, a `Vm` field or an inline cache.

### The heap

All heaps of a process share one reservation of address space (16 TiB,
inaccessible until committed, so up to 512 heaps alive at once); each
takes a 32 GiB slice of it
(`CLOISON_HEAP_LIMIT`, in MiB, lowers what it may use), committed 8 MiB at a
time. Sharing makes the metadata's and the cards' biases process
constants, so the barrier needs no heap at hand. A slice is 32 KiB
blocks:

- **Small blocks** hold cells of up to 8 KiB. A cell is a run of 16-byte
  granules and never crosses a block's end. The mutator allocates by
  bumping `ptr` against `end`, a hole of the current block; when the
  hole is full it takes the next hole of the block, then a block a
  collection left free granules in, then a free block, then a block
  never used. Holes under 256 bytes are left for later: each would send
  compiled code's inline allocation to the runtime after a few cells.
- **Large cells** get a run of whole blocks, reused once free.
- **Metadata**, beside the heap, one byte per granule
  (`meta_bias + addr / 16`): at a cell's first granule `YOUNG` (allocated
  since the last collection) or the color it was last marked with (an
  old cell), and `END` at an old cell's last granule; 0 elsewhere. A
  conservative word is a cell exactly when its granule's byte is `YOUNG`
  or a color; a hole is a run of granules no live cell covers, found
  from the bytes alone.
- **Cards**, beside the heap, one byte per 128 bytes, set by the barrier.

Memory is not zeroed when a hole is taken: each cell's maker writes all
a collection reads (an object's header and slots, `Value::EMPTY` in the
`create_object` stencils; an array's values), and zeroes what must
start zero (a byte buffer). Blocks a full
collection leaves empty are given back to the system (`madvise`), but
for as many as the heap will grow by before the next cycle.

### Cells

Every cell says how big it is from its first words (`heap::cell_size`):

| kind | first word | size from | references |
|---|---|---|---|
| object | its shape's address (8-aligned) | `inline_cap` (header bytes 10–11), plus a string's `elems_cap` units words or a function's upvalue count | inline slots; `props` and `elems` (arrays' payloads); a function's upvalues; a typed array's `code` (its buffer) and a cons string's `props` and `code` (its halves) |
| array of values | `VALUES \| capacity << 32` | its header | its values |
| byte buffer | `BYTES \| length << 32` | its header | none |
| upvalue | `UPVALUE` | fixed, 32 bytes | `closed` |

A pointer to an array or an upvalue points past its header word, at the
payload, so compiled code indexes `props` and `elems` as before. Two
pointers are interior: a flat string's `elems` points at its own units,
and a closed upvalue's `at` at its own `closed`; a typed array's `elems`
points into its buffer's bytes. None of them is ever followed as a
reference: the collector reaches the buffer through `code`.

### Generations

Young cells are those whose first granule says `YOUNG`. A **minor
collection** marks every young cell reachable from the roots, from the
young cells already marked, and from old cells on dirty cards, giving
each the current color and its `END` byte: it is old from then on, where
it is. The young cells left unmarked are garbage; their `YOUNG` bytes are
cleared, and their granules are holes. Nothing is copied and no pointer
is rewritten.

Promotion is immediate: a cell that survives one minor collection is
old. Premature promotion costs old-generation marking, which the
concurrent marker does on another core; an age bit (a second young byte
value, surviving one more minor collection) is the fallback if
measurement says the old generation fills with cells that die soon after.

### Roots

1. **The VM stack**, from its base to the highest word frames have
   reached: each word that is a cell by the metadata. A frame's registers
   are `Value`s; header slots hold Rust addresses (not in the heap) or
   the running closure (a root).
2. **The native stack and registers** (spilled at the collection's
   entry), from the collection's frame to the base recorded when the VM
   started running: each word that points into a cell, its first granule
   found by walking the metadata back. This covers values compiled code
   keeps in callee-saved registers, the slot registers a runtime-call
   wrapper pushes, and Rust locals.
3. **The VM's own references**: the global object and the builtins'
   prototypes and functions, the string constant table and
   `single_chars`, `Code::constants`, open upvalues, a pending error's
   value, the handle stack.
   Conservative scanning retains what stale words point to, and the VM
   stack had stale words where compiled code never writes: an inlined
   frame's registers, and registers the code keeps in machine registers,
   hold what an earlier frame at that depth left. On Splay one such word,
   in compiled `SplayRun`'s inlined area, held a node removed long before,
   which pointed at nodes removed after it: the live set grew from 1.3 to
   5.3 million cells and the heap to 1.4 GB. Compiled code now zeroes the
   registers dead at its entry, once per collection (a two-load check at
   entry jumps to a cold stub when the collection count changed: zeroing
   at every entry cost Richards 55%). Splay's live set stays at 1.3
   million cells and its heap at 640 MB.
4. **References from Rust structures**: inline caches' holders
   (prototypes, cached callees), shapes' prototypes, call and
   constructor sites, and cells compiled code embeds (each compiled
   function records them). A major collection enumerates them. A minor
   collection cannot afford to (there are hundreds of thousands of
   inline-cache ways in TypeScript), so the store of a young cell into
   one is logged (`Heap::note_external`) and the log is a root of the
   next minor collection, after which the cell is old.

### Safepoints

A collection runs only at a safepoint: allocation sets `gc_requested`
when the nursery's budget is used, and never collects. Safepoints are
the entries of the runtime functions that interpreter handlers and
compiled code call to allocate or to call (creating objects, arrays and
closures, `new`, calls into natives and through the runtime, the
allocation slow paths of the stencils), and the tier-up exit of a loop.
At those points every value is in a VM frame, a register or a native
stack word, all of which the collection scans. A native that keeps
values in a `Vec` across `call_function` (`sort`, `map`, `filter`,
`replace` with a function) holds them in a handle scope.

### The write barrier

Every store of a value into a cell, and of an array or upvalue pointer
into an object, runs the barrier (`heap::barrier`, and `barrier` in the
stencils):

```text
if v is a cell && meta(v) == YOUNG && meta(host) != YOUNG:
    card(address stored to) = 1
```

with `meta(a) = SIDE + (a >> 4)` and `card(a) = SIDE + (a >> 7)`: the
shared reservation is placed (at 16 TiB) so both side tables hang off
one constant, `SIDE` = 8 TiB, which arm64 builds in one `movz` and
indexes with a shifted register; no operand is patched in. (macOS grants
addresses asked for only from 8 TiB up, which rules out a zero `SIDE`.)
A number store leaves at the first test, and the stencils whose value is
known to be an int32 or a double have variants without the barrier
(`set_own_n`, `set_keyed_nc_n`, …); booleans, `null` and `undefined`,
which the JIT's types don't distinguish from cells, take the two tests.

The barrier's first layout cost as much as its instructions: LLVM merges
a stencil's tail calls into one `b cloison_continue` that every early exit
branched to, so a store of an old value took two branches. `build.rs`
now aims every branch to an inner `b <symbol>` at the symbol itself,
after which the stencil's exits are single branches to the next piece
(it also shortened a few stencils that had nothing to do with the
collector). On Richards, which allocates 40 MB and stores pointers
constantly, the barrier cost 27% at first and 13% now; the
microbenchmarks are 6% slower than without a collector, geomean, the
store-heavy ones up to 30% (most of it storing booleans and `null`). The first version marked the card unconditionally (two
instructions, no host or value needed) and it cost more than it saved:
every initializing store dirtied a card, and as young cells are
interleaved with old ones in reused blocks, nearly every card was dirty
at every minor collection, whose card scan then traced most old cells:
7.2 of RayTrace's 22 seconds. With the condition, RayTrace's 4,000
minor collections spend 0.9 seconds in all, and NavierStokes, which only
stores numbers, is back to its time without a collector.

Compiled code marks an object's own card for a store to any of its
in-object slots (no offset to add), Rust code the slot's; so a minor
collection scans the old cells' words on dirty cards and the whole of
an object that starts on one. It finds old cells from the metadata
alone (only their first granule has a color), eight granules at a time.
Every young cell reached is old afterwards, so every card is clean after
a minor collection.

The barrier is also what found the one store site the audit missed: with
`CLOISON_GC_CHECK_BARRIER`, each minor collection traces every old cell (what
a marking would follow, not the padding past an odd number of slots,
which no maker writes) for a reference to a young cell it left unmarked,
and the `set_keyed!` family of stencils, which writes `$v`, was one.

### The major collection

Concurrent, with two short pauses, the marker thread doing the rest
(`gc::Marker`):

1. **Start pause**: advance the current color, so every old cell is
   unmarked, and run a minor collection that *shades* as it goes: every
   old cell its roots, its survivors and its dirty cards refer to is
   marked and handed to the marker instead of traced. The Rust
   structures' references are shaded too. Survivors are marked with the
   new color, so they count as marked for the cycle.
2. **Concurrent mark**: the marker pops cells, marks their unmarked old
   referents (a byte store of the color; the `END` byte when it traces
   one) and pushes them; young referents are left for the final pause.
   The mutator's barrier compares a stored value's metadata with the
   heap's current color, kept in the metadata byte of the first granule
   of the heap's slice (a block of no cells): outside a cycle that is
   "the value is young"; during one it also catches an old cell not
   marked yet, so the store's card is dirty whichever kind it was. Minor
   collections meanwhile pause the marker, shade the old cells their
   survivors and dirty cards refer to (the cards are cleaned after, so
   what they recorded goes to the marker), and resume it. When it runs
   out of work it sets the heap's request flag.
3. **Final pause**: mark everything not marked yet, from the roots, the
   Rust structures, the dirty cards (old cells stored into since they
   were traced) and the marker's leftover work, young cells included.
   On Splay it takes 2.5 ms; marking in one pause took 30 ms there. If
   the heap outgrows twice its trigger before the marker is done, the
   final pause runs anyway and the mutator finishes the marking.
4. **Concurrent sweep**: the marker thread sweeps the small blocks from
   their metadata (the dead cells' bytes cleared, live bytes counted)
   and hands them back: blocks with room to allocation, empty ones to the
   free blocks, their memory to the system. Allocation meanwhile takes
   free and new blocks and the swept ones as they come, and sweeps a few
   blocks itself rather than take a new one while the sweeper is behind
   (JSC's sweep-to-allocate). While sweeping, a dead old cell still has
   the old color, so minor collections trace only old cells of the
   current one. The next cycle finishes the sweep first. Large cells are
   swept in the final pause, and so are the young cells it left
   unmarked, as a minor collection frees them: left `YOUNG` for the
   sweeper, a stale word a minor collection found meanwhile kept one as
   a live young cell, with what it referred to swept from under it
   (EarleyBoyer did so six times with a 1 MiB nursery).

`CLOISON_GC_STW` marks and sweeps full collections in one pause instead;
`CLOISON_GC_CYCLES` runs cycles back to back, and with `CLOISON_GC_VERIFY` each
final pause also walks every root again in one pass and panics on a
reachable cell the concurrent marking left unmarked.

Correctness of concurrent marking rests on four facts: nothing moves;
every word of a cell is a valid value or a header at every instant (the
mutator writes whole words, and growth writes the new array before the
pointer to it, the pointer's store dirtying the card); an array's
capacity is in the array, so a marker never pairs an old pointer with a
new capacity; and every store of a cell not marked with the current
color into an old cell dirties its card, which the final pause rescans.
The marker reads cells and `Code::upvalues` (immutable once loaded) and
writes only old cells' metadata, which the mutator writes only in
pauses.

### Pacing

- **Nursery size**: the bytes allocated between minor collections. Its
  cost is the roots and the survivors marked; a larger nursery gives
  cells more time to die but leaves the cache, and takes memory a small
  program never needs. It starts at 8 MiB and doubles, up to 32 MiB,
  whenever minor collections took over 2% of the time across the last
  four (see step 6, *Adaptive nursery*).
- **Old-generation headroom**: the next major cycle starts when the old
  generation has grown by its live size after the last cycle (64 MiB at
  least), counted two ways, whichever comes first: blocks in use since
  the sweep, and bytes minor collections promoted (allocation into
  recycled holes adds no blocks). Not yet: a CPU-share target, and
  `CLOISON_HEAP_LIMIT` steering it.
- **When marking starts**: earlier by what the last cycle grew the heap
  by while marking, so the heap peaks about where a stop-the-world
  collection would have let it.

`CLOISON_GC_STATS` prints each collection: kind, pause, bytes allocated
since the last, marked, freed, and the heap's size.

### Weak references, finalization

`WeakMap` and `WeakSet` keep their entries in a buffer tagged
`WEAK_VALUES`, whose tracing (a card scan's too) stops at its header: its
keys and values are reached only by the ephemeron pass every collection
runs once its marking is drained (`Vm::ephemerons`). Over the weak
collections still live (`Vm::weak_tables`, not roots), it marks the
value of each entry whose key is live and drains again, until no new
value is marked, then clears the entries whose keys are not live. A
minor collection counts old cells live, so it clears young keys' entries
only; entries stored while the marker runs are no different (the
barrier's card marks don't make them strong). `WeakRef` and
`FinalizationRegistry` don't exist yet. Rust-side resources reachable from cells
(`Regex` boxes) are freed by the sweeper when the cell dies; nothing else
needs a finalizer.

### Interaction with the JIT

- **Inline allocation** keeps its bump of `Heap::ptr` against `end`, and
  writes the new cell's metadata byte (`Heap`'s third word is the
  metadata's bias). The slow path is a runtime call, so a safepoint.
- **Stores** get the card mark as part of the stencil, elided by type.
- **Embedded cells** are recorded per compiled function and are roots;
  nothing the collector does invalidates code.

### What `object-model.md` keeps

Virtual objects at the bytecode level (A), compact layouts (B), the
measurement plan for DeltaBlue's allocation sites. The externalization
barrier, the external bit, the transitive walk and the two-heap invariant
go.

## Plan

Each step lands with its tests and a measurement against the previous
one: `scripts/gc-bench.sh` (time and peak RSS of Octane's deterministic
runs) and the microbenchmarks.

1. **Heap layout, no collection** (done). The reservation, blocks,
   metadata and cards, hole and large allocation, self-describing cells
   (`inline_cap` in the header, arrays' and buffers' header words,
   upvalue cells), the stencils writing the metadata byte;
   `CLOISON_GC_VERIFY` walks the heap and the test suite does after every
   run. Measured: +1.8% geomean on the microbenchmarks, +5 to 19% on the
   allocation-heavy ones (more memory per object, all of it fresh pages).
2. **Roots and marking** (done). The conservative scans, the VM's
   references, the log of cells Rust structures take, the handle scopes,
   safepoints. Validated with full stop-the-world collections that poison
   what they free without reusing it: all of Octane ran correctly through
   3,019 of them.
3. **The minor collection** (done). The barrier in the runtime, the
   interpreter and the stencils; card scanning; freeing dead young cells
   by clearing their metadata eight bytes at a time; holes and their
   reuse in recycled blocks; `CLOISON_GC_STRESS` (collect at every safepoint)
   and `CLOISON_GC_CHECK_BARRIER` (find an old cell referring to a freed young
   one). The nursery is 32 MiB (`CLOISON_GC_NURSERY`): at 4 MiB, RayTrace took
   18.3 s instead of 10.8 s, nearly all of it in compiled code's inline
   allocation falling to the runtime for the next small hole between
   survivors; at 32 MiB it runs at its speed without a collector.
4. **The full collection, stop-the-world** (done). Colors, sweeping,
   freeing empty blocks to the system, a trigger at the blocks in use
   after the last full collection plus the live size (64 MiB at least).
   Octane after steps 2 to 4 against no collector (Apple silicon, seconds and peak
   RSS):

   | benchmark | before | after |
   |---|---:|---:|
   | Richards | 1.58 s, 47 MB | 2.01 s, 41 MB |
   | DeltaBlue | 1.15 s, 1.2 GB | 1.12 s, 68 MB |
   | Crypto | 5.64 s, 784 MB | 5.64 s, 49 MB |
   | RayTrace | 10.77 s, 1.6 GB | 10.56 s, 58 MB |
   | EarleyBoyer | 28.7 s, 4.4 GB | 33.6 s, 191 MB |
   | RegExp | 18.6 s, 2.5 GB | 18.7 s, 77 MB |
   | Splay | 2.41 s, 4.9 GB | 5.43 s, 1.4 GB |
   | NavierStokes | 7.11 s, 10 MB | 7.17 s, 11 MB |
   | pdfjs | 2.39 s, 1.6 GB | 2.64 s, 324 MB |
   | GBEmu | 1.32 s, 189 MB | 1.39 s, 114 MB |
   | code-load | 2.71 s, 569 MB | 2.78 s, 513 MB |
   | Box2D | 1.69 s, 228 MB | 1.78 s, 121 MB |
   | TypeScript | 4.07 s, 534 MB | 4.51 s, 439 MB |

   (The "before" RSS undercounts: see "Numbers".) Splay spends 1.7 s
   collecting: its minor collections each mark about 220,000 survivors
   (4 ms), and its full collections mark 1.3 million cells (33 ms). Richards,
   which allocates 40 MB in all, is the barrier's cost: on arm64 each one
   builds two 48-bit table biases (six instructions) before its two loads.
5. **Concurrent marking and sweeping** (done). One marker thread, the
   two pauses of "The major collection", minor collections during marking
   shading their survivors, cards dirtied during marking rescanned in the
   final pause (no mod-union table or precleaning: the final pause stayed
   short without them), sweeping by the marker thread with the mutator
   sweeping the blocks it needs first. The barrier compares against one
   color byte per slice instead of building table biases. Pauses in ms
   (largest; `CLOISON_GC_STW` for the stop-the-world column):

   | benchmark | full (STW) | start | final | minor |
   |---|---:|---:|---:|---:|
   | Splay | 63.8 | 7.0 | 6.7 | 9.0 |
   | TypeScript | 49.9 | 15.9 | 1.1 | 16.2 |
   | EarleyBoyer | | 1.8 | 0.2 | 2.7 |
   | pdfjs | | 1.1 | 0.7 | 1.1 |

   Octane (seconds, peak RSS): Richards 1.85 s, 41 MB; DeltaBlue 1.06 s,
   65 MB; Crypto 5.59 s, 49 MB; RayTrace 10.20 s, 57 MB; EarleyBoyer
   29.9 s, 127 MB; RegExp 18.4 s, 76 MB; Splay 4.45 s, 911 MB;
   NavierStokes 7.13 s, 11 MB; pdfjs 2.60 s, 330 MB; GBEmu 1.36 s,
   114 MB; code-load 2.76 s, 513 MB; Box2D 1.73 s, 91 MB; TypeScript
   4.36 s, 469 MB. Splay's peak grows from 636 MB stop-the-world to
   911 MB: what dies while the marker runs floats until the next cycle,
   and allocation continues meanwhile; the headroom policy is step 6's.
   The start pause is a minor collection that also shades old cells the
   young ones refer to, so it costs what a minor collection costs; the
   nursery's survivors (Splay's 220,000 a collection, TypeScript's
   200,000) are the remaining pause, and the age bit of step 6 the remedy.
6. **Tuning** (done): the nursery size, the headroom, more marker threads, an
   age bit, and evacuation of fragmented blocks if measurement shows
   fragmentation costs memory (moving only cells no conservative word
   and no Rust structure refers to).
   - *Pacing* (done): a cycle starts earlier by what the previous one grew
     the heap by while marking (its lead), no less than half the room and
     64 MiB after a sweep. Splay's peak RSS 911-968 MB -> 760-820 MB at
     the same speed; at half of 64 MiB EarleyBoyer's doubled cycles cost
     12%, hence the floor. Splay's live size is 117-130 MiB, yet 372 MiB
     of blocks stay in use after a stop-the-world full collection (495
     MiB concurrent): fragmentation, not pacing, is most of its memory.
     After a stop-the-world full collection Splay's blocks are 31% full
     on average (2,900 of 12,000 under a tenth): the nursery allocates in
     every recycled block's holes and its survivors pin them all.
   - *Promotion trigger* (done): allocation into recycled blocks' holes
     adds no blocks, so a trigger on blocks in use let old garbage grow
     well past the room. A cycle also starts once minor collections have
     promoted the room's bytes since the last one. Splay 14 cycles -> 20,
     peak RSS 760-820 MB -> 646-659 MB at the same speed (4.0-4.2 s);
     nothing else changes.
   - *Nursery size* (kept at 32 MiB): at 16 MiB RayTrace 11.5 s and
     EarleyBoyer 31.1 s (pauses halve); at 64 MiB they gain 2-3% while
     TypeScript's largest pause doubles to 31 ms and Splay's peak grows
     to 810 MB.
   - *Adaptive nursery* (done): 32 MiB took memory small programs never
     use (Richards' peak RSS 40 MB, its live data under 1). Fixed sizes
     against 32 MiB, Octane score and peak RSS: at 4 MiB Richards 11 MB,
     Crypto 20 (from 46), RegExp 53 (from 79) and 3% faster, Splay 10%
     faster; but RayTrace -35%, EarleyBoyer -27%, DeltaBlue -9% (and 84
     MB, from 63: more promoted). RayTrace's loss is not its pauses (0.2%
     of its 4 MiB survives) but allocation: the few cells each minor
     collection promotes die soon after, no full collection runs below
     64 MiB, and the dead cells cut the recycled blocks into holes of
     ~700 bytes, 5,700 a nursery, each an `alloc_slow` (12.6% of its
     time). EarleyBoyer's is its pauses (23% of the time at 4 MiB, 8% at
     32). Minor collections' share of the time sorts them: under 2% at 4
     MiB for Richards, Crypto, RegExp, CodeLoad, GBEmu; 6-23% for
     RayTrace, DeltaBlue, PdfJS, EarleyBoyer, Splay. Hence the nursery
     grows on that share. Starting at 4 MiB cost Box2D 1.7% (its short
     run spent at 4 MiB); from 8, Octane +0.2% (RegExp +3.2%,
     RayTrace -0.8%, the garbage its early nurseries promoted); peak RSS
     Richards 40 -> 15 MB, Crypto 46 -> 22, RegExp 79 -> 56, Box2D 70 ->
     57, CodeLoad 185 -> 176. Faster growth (two collections sampled, or
     one) did not win RayTrace back. `CLOISON_GC_NURSERY` fixes the size.
   - *Not done, not warranted by measurement*: more marker threads (the
     marker finishes well inside a cycle: the lead is under a third of the
     room), an age bit (EarleyBoyer, the most promoting benchmark, spends
     2% collecting; Splay's survivors are long-lived and an age bit would
     mark them twice), and evacuation. Splay's fragmentation comes from
     deleting random nodes, so any placement pins blocks; evacuating
     would need the concurrent marker to record slots into candidate
     blocks and pin what conservative words and compiled code refer to,
     for one benchmark's memory (still a seventh of no collector's).

   Octane after step 6 (seconds, peak RSS): Richards 1.86 s, 41 MB;
   DeltaBlue 1.09 s, 64 MB; Crypto 5.60 s, 49 MB; RayTrace 10.85 s,
   57 MB; EarleyBoyer 29.9 s, 140 MB; RegExp 18.5 s, 77 MB; Splay 4.14 s,
   657 MB; NavierStokes 7.92 s, 11 MB; pdfjs 2.61 s, 330 MB; GBEmu
   1.37 s, 115 MB; code-load 2.75 s, 512 MB; Box2D 1.73 s, 90 MB;
   TypeScript 4.34 s, 486 MB. The collector's share of wall time is under
   3% everywhere but Splay (19%). `bench/micro` runs 6.5% slower
   (geomean) than with no collector: barriers, safepoints and
   once-per-collection register clearing.
   - *Prefetching* (done): marked cells pass through an eight-entry ring
     between the stack and tracing, prefetched on entering it. Splay's
     minor tracing 626 ms -> 450 ms, its collection share 23% -> 19.5%
     (sixteen entries: no better). Splay stays under the 90% target: its
     minor collections mark 220,000 survivors each, the tree's new nodes.
     TypeScript's largest minor pauses are cards (50,000 dirty, 18 MB of
     old objects traced through them): the compiler writes young values
     into that many old objects.
   - *Microbenchmarks* for what remains (`bench/gc/`, timed by
     `scripts/gc-micro.sh`; seconds, peak RSS, live at full collections,
     collecting's share of the time, longest pause):

     | file | s | MB | live MiB | gc | max pause | shows |
     |---|---:|---:|---:|---:|---:|---|
     | survivors.js | 1.0 | 171 | 19 | 13% | 5.4 ms | minor collections trace their whole nursery |
     | cards.js | 3.9 | 779 | 261 | 28% | 26 ms | dirty cards scanned in full, 23-26 ms of each pause |
     | fragmentation.js | 4.3 | 777 | 168 | 24% | 23 ms | random deaths in a non-moving heap: 4.6x live |
     | splay.js | 3.7 | 471 | 172 | 19% | 17 ms | Splay: survivors, 2.7x live, a 12 ms final pause |

     Card scanning dominates the longest pauses in all but splay.js,
     whose final pause retraces about 670,000 cells. (Measured with their
     loops in the interpreter: `%` with a register divisor kept them out
     of the JIT. Compiled, survivors.js takes 0.39 s, 37% collecting;
     cards.js 1.47 s, 21%, its pauses 32 ms with five times the stores per
     nursery; splay.js 2.7 s, 22%; fragmentation.js 3.1 s, 22%.)
   - *Card references prefetched* (done): the references on dirty cards
     pass through a 32-entry ring, their metadata prefetched on entering
     it (old cells' references go all over the heap, each read a cache
     miss). cards.js's longest pause 26 ms -> 16 ms, its collection share
     28% -> 19%; fragmentation.js 24% -> 17%, 23 ms -> 15 ms (16 entries:
     19 ms; 64: no better than 32). Octane unchanged. Not kept: the same
     ring for what traced cells refer to (splay.js 18% -> 21%: a cell's
     references are mostly near it, their metadata in cache already),
     prefetching the dirty cards' own words a few cards ahead (no change),
     skipping
     references into blocks the nursery did not allocate in (the nursery
     fills holes in thousands of recycled blocks, so few are skipped),
     subtracting the sweep's holes from the headroom (fragmentation.js
     777 -> 695 MB, but half of Splay's runs 80 MB higher: cycles start
     only once the holes are used up, so the lead lands in new blocks),
     and 128-byte holes (cards.js 758 -> 576 MB, fragmentation.js 696 ->
     594 MB, but RayTrace and Splay 6-7% slower with no gain on Octane's
     memory).
   - *Card scanning on several threads* (done), with three changes,
     each measured on cards.js's longest pause (33 ms with its loop
     compiled, of which 25 ms scanning cards):
     cards of 128 bytes instead of 512 (a random store dirties a fourth
     as much old memory: 25 ms; 256 bytes: 29 ms); runs of dirty cards
     scanned as one range (the cell reaching into a run looked for once,
     and cells started on one card not traced again from the next); and
     the dirty runs (split to 64 cards) scanned by up to eight threads
     once there are 4,096 dirty cards, each claiming a cell by a
     compare-and-swap of its metadata byte, in minor, start and final
     pauses alike (`CLOISON_GC_THREADS`; the marker is paused then). Taking
     one run at a time from the shared counter did not scale (11.2 ->
     9.4 ms on four threads); 256 runs a take does: the card phase
     18.8 ms on one thread, 12 on two, 9 on four (eight no better on four
     performance cores). cards.js 33 ms -> 16-19 ms, collecting 20% ->
     12.5%; fragmentation.js 20% -> 16%; TypeScript's longest minor
     pause 16.6 -> 14.9 ms, major 16.3 -> 13.7; Splay's minor 10.9 ->
     6.2 ms. Peak RSS +3% on cards.js (the smaller cards' table and the
     threads' stacks). What is left of the card phase is marking: cards.js
     finds 440,000 young survivors through 480,000 references on 320,000
     cards per minor collection.
   - *Large arrays marked in slices* (done): the marker took up to 16 ms
     to stop for a pause when tracing a large array (fragmentation.js's
     table of 1.5 million slots is one cell, traced without a check), and
     the pause waited for it. An array of more than 1,024 values is now
     traced that many at a time, the rest left on the mark stack as a pair
     (the next slot, the cell's address with bit 0 set), and the stop
     check counts work by slots. fragmentation.js's longest pause
     19.8-26.1 ms -> 8.3-10.0 ms over five runs each; nothing else
     changes.
   - *Survivors traced on several threads* (done): a pause (minor, start
     or final) traces on one thread until it has traced 16,384 cells,
     then on up to `CLOISON_GC_THREADS` (`Marking::drain_parallel`): each
     from its own stack, claiming cells as card scanning does, handing
     half its stack to a shared queue whenever that is empty
     (from 16 cells: a tree traced depth first keeps a stack about as
     deep as the tree, so a threshold on the stack's size never fired on
     Splay's), done when all are out of work; an array's slices stay with
     the thread that has them. The switch is by work done: at 2,048 cells
     DeltaBlue's minor collections (2,900 cells) paid for threads,
     1.8% -> 2.7% of its time collecting. splay.js's minor trace phase
     5.99 ms -> 1.99 ms on four threads (8: 1.90); collecting 21% ->
     10.7%; survivors.js 38% -> 27%; Splay 19.6% -> 12.8% (4.23 s ->
     3.80 s), longest pause 10 -> 4.5 ms; TypeScript's longest pauses 15
     -> 8.4 ms; the rest of Octane unchanged.
   - *The log of Rust structures' cells* (done): a cell `Heap::note`
     logs is a cell's first granule, marked as one, not resolved as an
     interior pointer (`keep`'s addresses, which may be, have their own
     log), and the same cell noted twice in a row is logged once
     (prototype caches note their holder at every fill: TypeScript logged
     386,000 cells for 31,000 between two collections). TypeScript's
     roots 27 ms -> 3 ms over its collections, its pauses 91 -> 67 ms in
     all, its longest 11.6 -> 7.8 ms.
   - *Not kept: parked threads for pauses.* Spawning seven threads costs
     170 microseconds a phase; a pool of parked ones made card scanning
     17-28% faster but tracing 10-25% slower, even traced on spawned
     threads after cards scanned on parked ones, whether the parked ones
     kept to the collecting thread's L3 (Linux, `sched_setaffinity`),
     spread over all the host's cores or were left where the
     scheduler put them: pauses in all within 10% either way, by file.
   - *Empty blocks kept; blocks recycled for their room* (done). Every
     block a sweep emptied went back to the system (`madvise`) and was
     faulted in again within the cycle: Splay took 444,000 page faults a
     run, 17% of its cycles in the kernel. Up to the room's bytes of
     empty blocks now stay committed, taken before fresh ones (their
     memory zeroed then; a released block's is zero, only its metadata
     is cleared). And a minor collection recycles a block for the room
     `free_young` left in it, counted while clearing the `YOUNG` bits
     (runs of at least two zero metadata words), not for the bytes
     allocated there: most of Splay's young cells survive, and
     allocation scanned 1.0G granules a run (33M holes too small) to
     find its 32 MiB a cycle, 0.5G (13M) now. Splay 5.05 s -> 4.09 s
     (keeping blocks alone 4.38 s; counting room alone was slower, more
     blocks faulted in), peak RSS 587 -> 685 MB (614 MB keeping only the
     headroom's bytes: 4.40 s); its pauses' free phase 74 -> 92 ms.
     Others within noise, their peak RSS unchanged.

7. **Later.** The header diet (shape as a 32-bit ID: 16-byte headers,
   most objects in one or two granules fewer), shapes as collectable
   cells, ephemerons with ES6.

## Risks

- **A missed root is a use after free.** `CLOISON_GC_STRESS` collects at
  every safepoint and overwrites freed cells, so a missed root crashes on
  first use; the test suite runs under it. The VM's references and the
  Rust structures are the manual part; the conservative scans cover the
  rest by construction.
- **A missed barrier is a use after free too**, found the same way. The
  barrier is one function in Rust and one macro in the stencils.
- **Conservative retention.** A stale native stack word can keep a dead
  cell (and what it reaches) alive for a cycle. Bounded by the stack's
  size; Whippet, JSC and Guile live with it.
- **Fragmentation.** Survivors pin their granules. Holes are granule-
  precise, so the waste is the survivors themselves and holes too small
  for the next allocation; `CLOISON_GC_STATS` reports free granules in
  partly used blocks so the cost is visible, and evacuation is step 6's
  remedy.
- **Concurrency bugs** are kept to the marker's reads of cells and
  writes of metadata; the verifier compares a concurrent mark with a
  stop-the-world one.

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
