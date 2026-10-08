# A heap sandbox for toi

Status: design only, **deferred**. The survey part was researched in
October 2026 from engine sources, papers and blogs (sources at the end).

## Decision: deferred

Not worth building yet:

- **No user needs it yet.** toi has no embedder running untrusted code.
  A sandbox only matters to an embedder running untrusted JS in-process,
  and only once an attacker already has an engine memory-corruption bug.
- **The heap is still moving.** The GC, strings, exceptions, closures and
  buffers are still missing. Each would need the sandbox's rules and an
  audit, and that is cheaper once they have settled.
- **The cost lands in the hot paths.** Expect 1–3% on property-heavy code,
  and an extra `add` on arm64 field loads. Without the testing and fuzzing
  of plan step 4, the guarantee would also be unverified.

What to keep in mind meanwhile, because it is cheap now and expensive later:

1. **Shape IDs** (plan step 1), as an ordinary optimization. The guard is
   `cmp m32, imm32` instead of `movabs` + `cmp`, 8 bytes instead of 13.
   Keep it if `perf.sh --ab` agrees.
2. **The allocator and GC rewrite.** Allocate from one reservation, and
   track positions as offsets or bounded indices from its base. Adding
   offsets after the GC exists would be the most expensive part of a later
   retrofit.
3. **No Rust addresses stored in heap objects.** Natives, host objects and
   strings reach Rust-side data through tables or IDs.

Revisit when toi is embedded to run untrusted code, gains Wasm, or nears
feature completeness. The plan below then still applies, from step 2.

## Goal and threat model

V8's sandbox model, adopted as is. **The attacker can read and write
anything inside the sandbox, arbitrarily**, for example through a JIT
type-confusion or a missed bounds check in toi. **They must not be able to
corrupt anything outside it.** The sandbox is a large virtual reservation
holding everything JS code can touch: objects, their properties and
elements, strings, and later buffers and Wasm memories. Everything else is
trusted: the VM, shapes, `Code` (bytecode, inline caches, feedback), JIT
code and its metadata, the VM stack and the native stack.

Sandbox bugs then reduce to two classes, both much smaller than "any bug in
the engine":

1. **An address computed from untrusted data that leaves the sandbox.** This
   is solved by construction: every in-sandbox reference is an offset, and
   every access is `base + zext32(offset) + bounded`.
2. **Trusted code misusing untrusted data**: lengths, enums, indices or
   handles read from the sandbox and trusted. This is solved by discipline
   and testing.

The sandbox is not a defence against bugs in trusted code itself: a bug in
`Shape` handling, the compiler, or the GC's metadata. It is also not
Spectre hardening. Both are out of scope here.

Pointer compression, storing references as 32 bits, comes almost free
once references are offsets. But it is a separate decision and optional
(see "Compression, optional").

## What others do

### V8: the reference design

**Pointer compression** (2020) came first.

- A 4 GB cage, aligned to 4 GB, with the base pinned in a register (`r14` on
  x64, `x28` on arm64).
- Tagged fields are 32 bits. Decompression is `movl r,[mem]; addq r,r14`
  (arm64: `ldr w; add x, x28, w, uxtw`).
- Smis are 31-bit, with "garbage" upper halves so decompression never
  branches.
- Heap down by up to 43%, renderer memory by up to 20%.
- The cost: doubles left fields (−3%) and Smis shrank (−1%). The first
  prototype was 35% slower on Octane until the JIT learnt to elide
  decompressions.

**The sandbox** (on by default since 2022) builds on that.

- **Address space.** A 1 TB reservation: 128 GB on Android, 16 GB on iOS.
  The 4 GB pointer cage sits at its start, followed by ArrayBuffers and
  Wasm memories, with 64 GB guards on each side and 224 GB more behind.
- **Guard sizing.** The guards are sized from the reach of the worst access:
  `base + offset + index × element size` for a typed array (32 GB + 8 ×
  32 GB). Where address space is short, indices are masked instead.
- **What in-sandbox fields may hold:**
  - *compressed pointers*;
  - *sandboxed pointers*: 40-bit offsets stored shifted left by 24, so the
    read `base + (raw >> 24)` can't escape;
  - *bounded sizes*: lengths stored shifted, at most 35 bits;
  - *handles*: 32-bit indices into tables outside the sandbox.
- **Tables.** Each is a fixed reservation sized so that `handle >> shift`
  can't exceed it, so there is no bounds check.
  - Entries carry a type tag and a GC mark bit.
  - The External Pointer Table (EPT) holds host pointers. The Trusted
    Pointer Table holds bytecode, `Code` and deopt data; since June 2026 it
    also replaces the Code Pointer Table.
  - The JS dispatch table holds each function's entry point *and parameter
    count*; it is write-protected with memory protection keys (pkeys).
  - The Wasm code pointer table holds entry points with signature hashes.
- **Trusted space.** Metadata that generated code relies on lives outside
  the sandbox: bytecode, `Code` metadata, deopt data, Wasm instance data.
- **Cost:** about 1% on Speedometer and JetStream (2024 figure).
- **Testing:**
  - a build exposes a JS `Sandbox.MemoryView` granting arbitrary
    in-sandbox read and write;
  - a SIGSEGV filter calls a crash harmless if it is in the sandbox or its
    guards, and a violation otherwise;
  - Fuzzilli has a sandbox mode, and a pkey "trap fuzzer" mutates values as
    they are loaded.
- **Bypasses so far, and their lessons:**
  - raw pointers left in sandboxed objects (Wasm instances, 2023–24), fixed
    by moving data to trusted space;
  - parameter counts read from in-sandbox objects, fixed by taking the
    count from the dispatch entry;
  - Wasm signature confusion, fixed with signature hashes;
  - double fetches of in-sandbox lengths;
  - in-sandbox lengths overflowing C++ buffers (`JSON.stringify`, `BigInt`,
    `toPrecision`);
  - in-sandbox enums hitting `switch` statements without a default.
  - A 2025 fuzzer (SbxBrk) found 19 bypasses.
  - V8's own conclusion: almost all are *first-order* memory bugs in
    trusted C++. That is exactly where memory-safe trusted code helps,
    unlike in JIT logic bugs.
- **Hardware:**
  - JIT pages and the dispatch tables are write-protected with pkeys on
    Linux x64, and with `pthread_jit_write_protect_np` on Apple.
  - An experimental "hardware sandbox" switches PKRU on entry to JS so that
    generated code can't write outside the sandbox at all. It is held back
    by stacks and signal delivery.

### JavaScriptCore

- **NaN-boxed values.** toi's encoding is JSC's. Cells are full 64-bit
  pointers; JSC never compressed them.
- **StructureID.** A 32-bit "compressed pointer" in every cell header.
  - It was first a table index.
  - In 2019 it was randomized ("entropy bits") against object faking.
  - It then became an offset into a structure heap caged to 4 GB.
  - Since December 2025 it is simply the low 32 bits of the Structure's
    address, with the cage mask removed as "of very low value as a security
    mitigation". With an attacker able to forge any ID, a cage over
    *trusted metadata* gives little: the forged ID still lands on a real
    structure, or in the middle of one.
- **Gigacage.** Masking, not compression: `base + (p & (size − 1))` at the
  use of a typed-array backing store.
  - Only the Primitive (buffer) cage remains, at 64 GB.
  - The JSValue (butterfly) cage was removed in 2024.
  - On arm64e, buffer pointers are also PAC-signed with the length as
    modifier.
- **JIT memory.** W^X per thread through `pthread_jit_write_protect_np`
  (APRR/SPRR on Apple silicon).
- **Overall.** JSC isolates *buffers*, the easiest primitive to turn into
  arbitrary read/write, and relies on PAC and process isolation for the
  rest. It has no general heap sandbox.

### SpiderMonkey

- NaN-boxing with a 47-bit payload. Unboxing XORs with the expected tag, so
  a speculative type confusion yields a non-canonical pointer.
- There is no pointer compression and no in-process heap sandbox; Firefox
  relies on process isolation (Fission). I found no recorded rationale.
- Its in-process sandboxing is for *libraries*: RLBox compiles C libraries
  to Wasm and back to C (wasm2c). It has shipped since Firefox 95, using
  Segue (below) on Linux x86-64.
- **Wasm memory32.** A 4 GB reservation plus only 32 MB + 64 KB of guard,
  with larger static offsets checked explicitly.
- **Wasm memory64** needs explicit checks: 10% to more than 100% slower,
  "2x just from changing your pointer size".

### Hermes

- 64-bit NaN-boxed `HermesValue` in registers.
- With compressed pointers, a 32-bit `SmallHermesValue` in object slots and
  arrays:
  - a 3-bit low tag and a 29-bit payload;
  - **doubles boxed on the heap**, small integers inline.
- Two decompression modes:
  - **contiguous heap:** `base + raw`;
  - **segmented:** `segmentMap[raw >> 22] + raw`, one load and one add,
    with 4 MB segments placed anywhere.
- This is the closest precedent for compressing a NaN-boxing engine: values
  stay 64-bit in registers and are narrowed only in the heap.

### HotSpot and Dart

- **HotSpot compressed oops:** `decode(n) = base + (n << 3)`, covering 32 GB.
  - *Unscaled* mode (heap below 4 GB) and *zero-based* mode (below 32 GB)
    drop the base.
  - Otherwise the base is pinned in `r12` and folds into addressing
    (`[r12 + n*8 + disp]`).
  - Class pointers are compressed separately, into a class space.
  - Lilliput (JDK 25, opt-in) packs a 22-bit class pointer into a 64-bit
    header: 22% less heap and 8% less CPU on SPECjbb.
- **Dart:**
  - **Layout:** one 4 GB-aligned region shared by every isolate group.
  - **x64 decoding:** `movl; addq r, [THR + base]`, which adds no pinned
    register at the cost of a load.
  - **arm64 decoding:** one register, `HEAP_BITS`, packs the heap base's high
    half *and* the write barrier's mask, so one pinned register serves two
    purposes.
  - **Result:** heap down 20–30%; no throughput figures published.

### WebAssembly runtimes

- **Isolation by guard pages.** Memory32 accesses are
  `base + zext32(index) + offset`. Reserving 4 GB plus a guard covering the
  largest folded offset makes every access safe without a check. An
  out-of-bounds access faults, and the trap handler turns the fault into a
  Wasm trap. The guards used:
  - V8 reserves 8 to 10 GB per memory, and runs out of address space at
    about 100 memories.
  - Wasmtime now reserves 4 GB plus 32 MB of guard, plus a 2 GB guard
    *before* the memory against sign-extension codegen bugs.
  - SpiderMonkey uses 4 GB plus 32 MB + 64 KB.
- **Spectre.** Explicit checks are paired with `cmov`/`csel` guards that
  turn an out-of-bounds address into an unmapped one. Cranelift's aarch64
  `load(iadd(base, ishl(idx, k)))` miscompile (CVE-2026-34971) shows how
  error-prone folding these address modes is.
- **Wasmtime's pooling allocator** keeps fixed slots and initializes
  memories copy-on-write. SpiderMonkey.wasm instantiates in about 5 µs
  instead of 2 ms.
- **Wasmtime's GC heap is itself a linear memory.** GC references are 32-bit
  indices into it, so a collector bug can't escape. That is the same idea
  as this document, applied to Wasm GC.
- **Segue** (ASPLOS '25) puts the linear memory's base in the x86-64 `gs`
  segment base and accesses `gs:[e32 + off]` with an address-size prefix.
  - That frees the base register and an addressing-mode slot.
  - It removes 44.7% of Wasm's overhead on SPEC CPU 2006, and 68–75% in
    Firefox's sandboxed libraries.
  - It needs FSGSBASE (Linux 5.9+), so it is not available on Windows or
    macOS.
  - wasm2c with the base merely kept in a local recovers much of the same
    gain.
- **ColorGuard** stripes neighbouring sandboxes across MPK colours, so a
  neighbour's memory acts as the guard: up to 15× more instances per
  address space. A PKRU write costs tens of cycles.
- **Software fault isolation (SFI)** gives the cost of sandboxing *arbitrary*
  code, an upper bound for toi.
  - NaCl x86-64 used `r15` as the base, a 4 GB sandbox with 40 GB guards,
    and 32-byte bundles, at 5–7%.
  - LFI on arm64 (ASPLOS '24, now an LLVM target) uses 4 GB sandboxes
    aligned to 4 GB, 80 KB guards, and the base in `x27`/`x21`.
    - Every access becomes `ldr x, [xBase, wN, uxtw]`, which is free, or
      `add w; ldr`.
    - Cost: 6.4–7.3% on SPEC 2017, about 2% to guard only stores, and +13%
      code size.

### Hardware

- **x86 MPK/PKU:** 15 usable keys and a per-thread `WRPKRU` register. This
  host has `pku` and `ospke`. It is useful for write-protecting JIT code
  and trusted tables, and later for a hardware sandbox mode as in V8.
- **arm64 POE:** the same idea with 8 keys; little hardware ships it yet.
- **MTE:** 4-bit tags on 16-byte granules. It is too slow and too coarse to
  serve as a sandbox (up to 5.6× in the worst cases measured in 2026), and
  TikTag leaks tags speculatively.
- **TBI** (arm64 top-byte ignore) lets tags ride in pointers for free, but
  conflicts with PAC and MTE.
- **CHERI** (128-bit capabilities) is research only, and incompatible with
  compression.

### What carries over

1. **Offsets, not pointers, in the sandbox.** The real boundary is that
   every in-sandbox reference decodes as `base + zext32(x)`. Compression is
   a separate decision: V8 coupled the two; Wasmtime's GC and LFI don't.
2. **Guard regions instead of checks.** Size the reservation so the largest
   address an access can compute from untrusted values still lands in it.
   This is Wasm's trick, V8's typed-array trick and LFI's trick.
3. **Trusted metadata stays out, reached through tables.**
   - Shapes, code and host pointers are 32-bit handles into tables outside
     the sandbox.
   - Each table is reserved at a fixed size, so `handle >> shift` (or
     `& mask`) needs no bounds check.
   - Entries are typed: a forged handle reaches some valid entry of the
     right type, never raw memory.
   - JSC's experience: don't bother caging trusted metadata; take it out.
4. **Facts the generated code relies on come from trusted state**, never
   from the sandbox: arity, entry points, types.
5. **Trusted code reads untrusted data once, and validates it.** Lengths,
   enums and indices from the sandbox are the remaining bug source.
6. **A test mode that grants arbitrary in-sandbox writes, plus a crash
   filter**, from the start.

## toi today

- **Values** are NaN-boxed with JSC's encoding; cells are raw 48-bit
  addresses (`src/value.rs`).
- **Object headers** (`src/object.rs`) are 48 bytes of raw pointers:
  `shape`, `props`, `elems`, `code`, plus `class`, `flags` and the lengths.
- **The heap** is a bump arena of `mmap`ed chunks.
- **Shapes** are `Box<Shape>` in the Rust heap: trusted, and already outside
  any future sandbox. They hold `Vec`s, `RefCell`s and field-type cells.
- **`Code`** is likewise trusted, and holds the bytecode, the inline caches
  (shape pointers and `Value`s), and the JIT's code.
- **The VM stack** (register files and frame headers) is a Rust allocation,
  so it is trusted too. Frames hold `Code` pointers and return `pc`s, which
  must never move into the sandbox.
- **Stencils** dereference objects directly:
  - `guard_shape` compares the header's first word with a 64-bit hole, which
    on x86-64 is a `movabs` plus a `cmp`;
  - fields are `[o + imm]` and `[[o+16] + imm]`;
  - keyed loads check `[o+32]` and load `[[o+24] + 8i]`;
  - `array_length` reads the `class` byte.
- **Pinned registers in compiled code:**

  | Register | x86-64 | arm64 |
  |---|---|---|
  | `acc` | r12 | x20 |
  | `regs` | r13 | x21 |
  | `tag` | r14 | x22 |
  | `ctx` | r15 | x23 |

  - **Slot registers:** x86-64 has six GPR slots, which is tight; arm64 has
    twelve.
  - **The interpreter:** its six arguments fill x86-64's tail-call
    registers. VM-wide state lives in the frame header (`regs[-VM]`, …).
  - **`ctx`:** points to the *exit cell*, a `u32` local to the JIT driver
    (`NO_EXIT` while code runs, so nested entries can share one cell). Direct
    calls pass it along unchanged.
- **Single-threaded.** toi has no workers or shared memory, so the
  concurrent double fetches behind several V8 bypasses can only come back
  through reentrancy (a getter or native that changes the object between
  two reads).

What would leave the sandbox today, if objects were inside one: cell
`Value`s, `shape`, `props`, `elems` and `code`. Everything else in the
header is data.

## Design

### Layout: one sandbox per VM

```text
   B - 4 KiB        B                      B + 4 GiB              B + 40 GiB
   ┌──────────┬────────────────────────────┬──────────────────────┐
   │ VM page  │ heap cage                  │ guard (PROT_NONE)    │
   │ trusted  │ objects, props, elements,  │ 36 GiB               │
   │ RW       │ strings; page 0 unmapped   │                      │
   └──────────┴────────────────────────────┴──────────────────────┘
```

- **One reservation per `Vm`**, aligned to 4 GiB, made with `MAP_NORESERVE`
  and `PROT_NONE`. Pages are committed as the heap grows, as the chunks are
  today.
  - The 47-bit user address space holds about 3,000 such sandboxes, plenty
    for parallel tests.
  - Per-VM sandboxes isolate VMs from each other as well, as V8's isolate
    groups do.
- **The guard is sized from the reach of the worst access.**
  - An access is `B + off + 8·i + d`, where:
    - `off` is a 32-bit offset;
    - `i` is a 32-bit index checked against an untrusted length, so in
      effect unchecked;
    - `d` is a stencil displacement below 64 KiB.
  - The furthest it reaches is `4 GiB + 32 GiB + 64 KiB`.
  - Typed arrays with 8-byte elements reach the same, if their lengths are
    stored as `u32`. Bigger buffers need V8-style bounded sizes, or masking.
  - **Nothing goes in front of the cage.** Offsets are unsigned, and
    `zext32` is part of every decode, so no access can go below `B`. Unlike
    V8 and Wasmtime, toi needs no front guard.
- **Page 0 of the cage stays unmapped.** The immediates (`null` = 2,
  `false` = 6, `undefined` = 10, …) and `EMPTY` = 0 decode to it, so
  treating one as an object faults inside the sandbox, which is harmless.
- **The VM page holds hot per-VM trusted state.** It is the page just below
  `B`, outside the cage, at offsets an in-sandbox address can't reach:
  - the exit cell;
  - the bump pointer and its limit (today reached through `Heap::bump`'s
    `imm64` address);
  - the epochs;
  - the table bases.

### The base register costs nothing

V8 and HotSpot spend a register on the base, and Dart spends a load. toi has
`ctx`, which points to the exit cell. With the exit cell at `[B − 4]`, **`ctx`
becomes the base**:

- exit stubs write `[r15 − 4]` instead of `[r15]`;
- the enter wrapper's `cmp dword [r15], NO_EXIT` becomes `[r15 − 4]`;
- everything else keeps its register.

On x86-64 an access is then `mov rax, [r15 + rcx + d]`: the same instruction
count as today, and one byte longer (an index in the SIB byte). On arm64
it is `ldr x0, [x23, w1, uxtw]` with no displacement, which is free.
With a displacement it is `add x9, x23, w1, uxtw; ldr x0, [x9, #d]`, one
more instruction. LFI and Dart pay the same.

Because the exit cell becomes per-VM rather than per-entry, nested entries
still work: the cell is `NO_EXIT` whenever code runs, and the driver resets
it after reading it, as it does today.

**The interpreter** has no free argument register on x86-64. It loads `B`
from a new frame-header slot `regs[-BASE]`: one L1 load per object access,
usually hoisted by LLVM within a handler. arm64 can afford a seventh
argument. `gs`-relative addressing (Segue) would remove even that load on
Linux x86-64, but Rust can't emit `gs:` operands outside inline asm, and it
makes the base per-thread state. It isn't worth it unless the frame-slot
load shows up in measurements.

### References inside the sandbox

| Today | In the sandbox | Decode |
|---|---|---|
| cell `Value` = 48-bit address | cell `Value` = 32-bit offset, upper 32 bits zero | `B + zext32(v)` |
| `props`, `elems`: `*mut Value` | `u32` offsets | `B + zext32(o)` |
| `shape: *const Shape` | `ShapeId(u32)` | `SHAPES[id & mask]` (trusted table) |
| `code: *const Code` | `CodeId(u32)` | `CODES[id & mask]` (trusted table) |
| `class`, `flags`, lengths | unchanged, **untrusted** | read once, validated |

**Values.** The encoding keeps JSC's layout; cells just shrink to the low 32
bits. Two rules make register values safe without an extra instruction:

- **The cell check also tests bits 32–47.** Today `is_cell` tests only
  `NOT_CELL_MASK`. The new check is
  `v < 2^32 && v & 2 == 0` (numbers are all ≥ 2^49, so the first test
  rules them out as before). A value that passed it, or that the type
  lattice derived from one that did, can be used as an index without
  `zext`.
- **A value loaded from the sandbox with a type the JIT trusts** (a field
  typed `Obj(shape)`, elements typed objects) is loaded as 32 bits:
  `mov ecx, [..]` zero-extends for free, and arm64 has `ldr w`. The field's
  type is trusted, but the field's contents are not.

Together, every cell in a register has zero upper bits, and `[B + v + d]`
is safe as is.

Today's check is three instructions on x86-64: `mov rax, r14; or rax, 2;
test rax, rdi`. The tighter check needs a mask with bits 32–63 and bit 1
set, which is not an `imm32`. That leaves two options, to be measured
against each other:

- **Tighten the check.** For example `mov eax, edi; cmp rax, rdi` plus a
  bit-1 test. It can cost one more instruction per check, and then nothing
  per access.
- **Keep the check, and `zext` at each access.** That is `mov eax, edi`,
  which the rename stage usually eliminates, before `[r15 + rax + d]`. It
  costs one more instruction per access.

On arm64, accesses with no displacement get the `uxtw` for free either way. The interpreter and the runtime decode through one function,
`cage.at(v) = B + (v as u32)`, which truncates unconditionally.

**Shapes become IDs.** Inline caches, transitions and the JIT keep using
`*const Shape` (trusted). Objects store a `ShapeId`.

- The table is a fixed reservation outside the sandbox, sized to a power of
  two. Unused entries point to a dummy shape, so `id & mask` always yields
  a valid shape and needs no bounds check.
- A forged ID makes an object use *another real shape*. Its slots are then
  read at that shape's offsets: still inside the cage, or in the guard, so
  harmless.
- **The shape guard gets cheaper**: `cmp dword [r15 + rcx], imm32` instead of
  `movabs rax, imm64; cmp [rcx], rax`. On x86-64 that is one instruction and
  about eight bytes less per guard, and on arm64 one fewer `movk`.
- `array_push` reads `elems_code` through the shape. It would read it through
  the table, or `elems_code` could move into the header word next to the ID.
- JSC's lesson applies: don't cage shapes. JSC made StructureIDs addresses
  inside a structure heap and found the cage of little value. IDs into a
  table of whole entries are the version that holds up when IDs are forged.

**Functions.** Functions carry a `CodeId`. Calls through the runtime look it
up, and take the arity and entry point from the trusted `Code`, never from
the object; V8 learnt this through its parameter-count bypasses. Direct calls
in compiled code are already sound: they compare the callee `Value` with a
known function object and jump to a baked-in entry. The comparison now uses
an offset.

**Class byte.** The class is decided by the shape, which is trusted.
`array_length` may keep its fast byte check, since its only consequence is
an in-sandbox load. Rust code must stop materializing `Class` from the byte
with a cast: an invalid discriminant is undefined behaviour, exactly V8's
"enum without a default" class of bug. The class comes from the shape, or
from `Class::try_from(u8)`.

**Host pointers.** Natives that keep Rust objects, timers and future host
objects go through an external pointer table: typed entries, a handle in the
object, and a mismatched type tag yielding a null pointer. Nothing needs this
yet. The rule now is simply: **no Rust address is ever stored in the
sandbox.**

### What stays trusted

- **The VM stack and frames:** register files hold `Value`s, which are
  offsets now; headers hold `Code` and `pc`s.
- **`Code`:** bytecode, constants and inline caches.
- **Shapes and field-type cells:** the JIT relies on them for every type it
  assumes.
- **JIT code, exit metadata and deopt (`Snapshot`).**
- **The heap's allocation state:** the bump pointer is in the VM page; mark
  bitmaps come later.

A future GC fits inside this:

- It traces with trusted shapes and untrusted lengths, and writes only
  through `B + offset`.
- Mark bitmaps outside the sandbox are indexed by `offset >> 3`, so they are
  bounded at 64 MiB of address space, like V8's tables.
- Fooling the GC into freeing a live object (V8 calls this acceptable) only
  produces in-sandbox aliasing.
- `docs/object-model.md` splits the heap into internal and external regions.
  Both live in the cage. "Is it internal?" can become an offset-range
  compare instead of a header bit, which is cheaper and can't be forged.

### Discipline for trusted Rust code

Most V8 bypasses are here, and Rust helps only partly: bounds checks protect
Rust collections, not raw cage pointers.

- **Use `Untrusted<T>` accessors for lengths, capacities, the class and
  flags.** They wrap the value read once from the sandbox, and use requires
  an explicit validation (`.below(cap)`, `.class()`).
- **Size Rust allocations by trusted counts where possible.** Never copy into
  a fixed-size Rust buffer by an in-sandbox length.
- **Read in-sandbox memory with raw pointer reads, not references.**
  `&[Value]` slices made from untrusted lengths are fine when every address
  is `B + …` (accesses end in the guard), but they invite optimizer
  assumptions.
- **Reentrancy:** a native or getter can change an object between two reads.
  Read once.

### Testing

- **Feature `sandbox-testing`** adds three natives:
  - `%SandboxRead(off)` and `%SandboxWrite(off, bits)` give arbitrary 64-bit
    access to the cage;
  - `%AddressOf(o)`.
- **A SIGSEGV handler classifies faults.**
  - A fault inside `[B, B + 40 GiB)` is a harmless crash: exit 0 in testing
    mode, as V8's `--sandbox-testing` does.
  - Anything else is a sandbox violation: exit with a distinct code.
- **Corruption fuzzing.** Random in-sandbox writes during the existing tests
  and benchmarks (corrupt headers, lengths, shape IDs, offsets) must never
  produce a violation. A load-site mutation mode, as in SbxBrk, can come
  later.

### Compression, optional

Once references are offsets, storing them in 32 bits is a layout choice:

- **Headers:** the header goes from 48 to 32 bytes, as below. A two-field
  object shrinks from 64 to 48 bytes, a quarter less memory touched. That
  matters, since page faults are a fifth of JIT time and allocation is a
  large share of the object benchmarks.

  ```text
   0  shape id u32 | class u8 | flags u8 | _ u16
   8  props u32    | props_cap u32
  16  elems u32    | elems_len u32
  24  elems_cap u32| code id u32
  32  inline slots …
  ```

- **Value slots** stay 64-bit NaN-boxed: unboxed doubles in fields are a
  large part of toi's numeric speed. Hermes' `SmallHermesValue` (32-bit
  slots with doubles boxed on the heap) is the alternative if memory ever
  matters more than doubles. That would be a separate, much larger
  decision.

The header layout is worth taking together with the sandbox. Value width is
not.

### Wasm and buffers, in the same frame

The one invariant is: **trusted base + `zext32`(untrusted) × scale + bounded
constant, inside a reservation that covers it**. It covers the JS heap
above, and also:

- **ArrayBuffers and typed arrays:**
  - backing stores live in the cage, or for large ones in a second region of
    the same reservation;
  - their data pointer is an offset;
  - lengths are `u32` element counts.
- **Wasm memories** (if toi ever runs Wasm):
  - each memory is its own reservation of 4 GiB plus a guard covering the
    largest folded offset, as V8, Wasmtime and SpiderMonkey do;
  - the memory's base is kept in trusted instance data, and JS objects refer
    to the memory through a table handle;
  - memory64 needs explicit bounds checks, as everywhere.
- **Wasm GC:** objects can be ordinary cage objects, as V8 does. Wasmtime
  reaches the same result by making its GC heap a linear memory.

### Hardening later

All of these are optional.

- **pkeys.** JIT code is RX after sealing, but trusted tables are writable.
  Write-protecting them with pkeys costs one `WRPKRU` around the rare
  writes. This host supports it.
- **A hardware sandbox mode.** While JS runs, PKRU denies writes outside the
  sandbox; the VM stack and VM page are tagged with an extension key. That
  catches bugs in trusted code too, which the software sandbox can't, but
  costs a PKRU write on every runtime call.
- **Spectre guards** (`cmov` on bounds checks), only if toi ever runs
  mutually distrusting code in one process.

## Costs, and how to measure them

| Path | Today | Sandboxed |
|---|---|---|
| x86-64 field load | `mov rax, [rcx + d]` | `mov rax, [r15 + rcx + d]` (+1 byte; base + index addressing may add a cycle of latency on some cores, so measure chains) |
| arm64 field load | `ldr x0, [x1, #d]` | `add x9, x23, w1, uxtw; ldr x0, [x9, #d]` (+1 instruction), or free at `d = 0` |
| shape guard | `movabs rax, imm64; cmp [rdi], rax` (13 bytes) | `cmp dword [r15 + rdi], imm32` (8 bytes) |
| cell check | `mov; or; test` | tests bits 32–63 too (≤ 1 instruction), or a `zext` per access instead |
| interpreter object access | as now | + `regs[-BASE]` load, usually hoisted |
| out-of-line and elements | two dependent loads | the same, plus base-indexed addressing |

The target is V8's: about 1% of cycles over the 77 benchmarks with
`perf.sh --ab`, with no regression beyond noise on the property-heavy
benchmarks (DeltaBlue-class). arm64's extra `add` per displaced access is
the most likely cost. If it shows, the stencils for repeated accesses to one
object can keep the decoded address in a scratch register within a fused
stencil.

## Plan

Each step builds on its own and is measured with `--ab` before the next.

1. **Shape IDs and the trusted shape table.** The header stores a
   `ShapeId`; guards become `cmp m32, imm32`; the class is read from the
   shape in Rust. This is useful without a sandbox (smaller guards) and
   tests the table decode on its own.
2. **The reservation and the VM page.** The heap allocates from a 4 GiB cage
   with the guard behind it. The exit cell, bump pointer and epochs move to
   the VM page, and `ctx` becomes `B`. Pointers stay raw, so behaviour is
   unchanged; this measures only the layout and the `ctx` change.
3. **Offsets.**
   - Cells become offsets, and `props`/`elems` `u32` offsets.
   - `code` becomes a `CodeId`.
   - Every dereference goes through `cage.at`, or `[r15 + v + d]` in
     stencils.
   - The cell check is tightened, and trusted-typed loads are 32-bit.
   - Optionally the header shrinks to 32 bytes in the same step.

   This is the main cost step.
4. **Discipline and testing.** `Untrusted<T>`; an audit of `vm.rs` and
   `interp.rs` for reads of lengths, class and flags; the `sandbox-testing`
   feature, the fault classifier and corruption fuzzing.
5. **Later, as features arrive:** the external pointer table (host objects),
   bounded buffers and typed arrays, Wasm memories, pkey protection of
   tables, and a hardware mode.

**Ordering with the GC.** The planned allocator and GC rewrite
(`docs/future-work.md`, "To revisit with the allocator rewrite and the
GC") should be written against offsets and the cage layout. Steps 1–3 are
best done before it, or together with it, rather than after.

## Sources

- V8:
  - sandbox README and glossary: <https://chromium.googlesource.com/v8/v8.git/+/refs/heads/main/src/sandbox/README.md>
  - <https://v8.dev/blog/sandbox>
  - <https://v8.dev/blog/pointer-compression>
  - `docs/heap/pointer-compression.md`
  - the commit replacing the code pointer table with the trusted pointer table: <https://chromium-review.googlesource.com/c/v8/v8/+/7726282>
  - SbxBrk: <https://arxiv.org/abs/2509.07757>
- JavaScriptCore:
  - `StructureID.h`, `Gigacage.h` and `JSCJSValue.h` in WebKit `main`
  - WebKit bugs 304017, 271006, 233379 and 174727
- SpiderMonkey:
  - <https://spidermonkey.dev/blog/2025/01/15/is-memory64-actually-worth-using.html>
  - RLBox: <https://hacks.mozilla.org/2021/12/webassembly-and-back-again-fine-grained-sandboxing-in-firefox-95/>
- Hermes: `include/hermes/VM/SmallHermesValue.h` and `PointerBase.h`.
- Dart: `runtime/docs/compressed-pointers.md`.
- HotSpot: JEP 450 and JEP 519.
- Wasmtime:
  - <https://docs.wasmtime.dev/api/wasmtime/struct.Config.html>
  - <https://bytecodealliance.org/articles/wasmtime-gc>
- Segue and ColorGuard (ASPLOS '25): <https://cseweb.ucsd.edu/~dstefan/pubs/narayan:2025:segue.pdf>
- LFI:
  - ASPLOS '24 paper: <https://www.scs.stanford.edu/~zyedidia/docs/papers/lfi.pdf>
  - LLVM target documentation: <https://llvm.org/docs/LFI.html>
- MTE overheads: <https://arxiv.org/abs/2601.11786>
