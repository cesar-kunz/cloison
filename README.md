# toi

A JavaScript engine in Rust: an ES5 front end, a register bytecode
interpreter, and a copy-and-patch JIT for x86-64 and arm64 (Linux, and macOS on Apple silicon) that
grows into a mid-tier optimizing compiler without an IR beyond bytecode.

```sh
cargo build --release
target/release/toi bench/micro/87-constraint-solver.js   # prints the completion value
target/release/toi --dump file.js                         # the bytecode instead
```

Status (2026-10-10): every Octane benchmark runs, with a generational
garbage collector (its old generation not yet marked concurrently). The
language is ES5 in sloppy mode. See
[Where it falls short](#where-it-falls-short).

## How it works

- **Values** are NaN-boxed into 64 bits with JavaScriptCore's encoding
  (`src/value.rs`): pointers as is, int32s under a tag, doubles offset by
  2^49.
- **The interpreter** (`src/interp.rs`) is a tail-calling dispatcher: all
  state lives in `preserve_none` arguments, so in registers, and each
  handler ends in `become` to the next. A second handler table encodes
  "the accumulator is an int32" so hot arithmetic skips type checks.
  Comparison-and-jump pairs are quickened into superinstructions.
- **Objects** (`src/object.rs`, `src/vm.rs`) have shapes in transition
  trees, in-object slots, inline caches that record field types and
  element kinds, and a global prototype epoch that invalidates cached
  prototype-chain loads. Closures use Lua-style upvalues.
- **The JIT** (`src/jit.rs`, `stencils/stencils.rs`, `build.rs`) is
  copy-and-patch: `build.rs` compiles the stencils into an ELF object,
  extracts each one's machine code and relocations, and the JIT copies
  one stencil per bytecode instruction and patches the holes. On top of
  that:
  - forward type inference over `I32 < F64 < Any`, with int32 arithmetic
    that side-exits on overflow and widens;
  - VM registers kept in machine registers for a whole region, as
    unboxed int32s or doubles (6 GPR slots on x86-64, 12 on arm64, 7 FP
    slots on both);
  - fused stencils for common sequences (`i += k; if (i < n)` is four
    instructions), and immediates folded into instructions through
    `asm goto`;
  - tiering from the interpreter by on-stack replacement at loop back
    edges, with a version cache keyed by the live values' types;
  - inline caches, polymorphic calls, inlining with a budget, scalar
    replacement of inlined constructors, direct calls between compiled
    functions, and guards and constant loads hoisted out of loops;
  - on arm64, register fields are patched while copying, so 1,844
    stencil variants collapse into 146 table entries, each verified
    byte-for-byte against LLVM's own compile of every variant.

- **Numbers print and compute as in V8.** `Math`'s transcendental
  functions (`src/math.rs`) are a port of fdlibm, which V8 ships, rather
  than the platform's `libm`, whose last bits differ between systems
  (Apple's `tan(1)` against glibc's): `sin`, `cos`, `tan`, `asin`,
  `acos`, `atan`, `atan2`, `exp`, `log` and `pow` are bit for bit Node's
  on x86-64, checked on 57,000 inputs over every reduction path. Node's
  arm64 builds let the C++ compiler fuse multiply-adds in the same code,
  so about 0.5% of their results differ by one ulp; the tests' expected
  values come from x86-64. Number to string picks the closest of the
  shortest round-tripping digit strings, ties to even, as ECMA-262 says
  and Rust's own formatting doesn't; checked on 22,000 doubles against
  Node.

`docs/future-work.md` is the design log: each step with what it took,
what it measured, and what did not work. `docs/gc.md` designs the
collector (non-moving and generational, cells in 16-byte granules with a
metadata byte each, conservative native roots, a card-marking barrier,
concurrent marking of the old generation) and tracks its implementation,
`docs/object-model.md` the object model and its escape analysis,
`docs/direct-calls.md` the call convention, and `docs/sandbox.md` surveys
heap sandboxing and defers it.

## Results

All numbers are from an Ubuntu Linux host, recorded in the docs; they
are the author's, not reproduced on other hardware.

Octane scores, toi against V8 14.6 with all tiers (higher is better):

| benchmark | toi | V8 |
|---|---:|---:|
| DeltaBlue | 58,713 | 118,616 |
| Richards | 35,775 | 62,785 |
| GameBoy | 40,757 | 124,442 |
| Crypto | 16,043 | 50,085 |
| Box2D | 15,522 | 132,428 |
| TypeScript | 26,377 | |

Inside a long DeltaBlue run the JIT is about 7x the interpreter and about 2.3x
behind V8's Maglev tier.

Code size is tracked per commit in `bench/sizes.tsv`: the release
binary is about 3 MB, stencil code about 98 KB.

## Testing

Every test program runs through seven engine configurations (the
interpreter, the JIT at each milestone, and tiered with tiny thresholds)
and the results must agree; `bench/micro/` and `tests/octane/` carry
Node's value as the reference. Several tests are mutation checks that
fail when a specific conversion or undo piece is removed.

```sh
cargo test --release
node bench/run.mjs --engine target/release/toi        # every micro against `expect`
cargo test --target aarch64-unknown-linux-musl         # arm64 under qemu-aarch64
```

Scripts in `scripts/`: `stencil-listing.py` lists every stencil as the
JIT emits it, with instruction counts (`docs/arm64-targets.md` holds
them against handwritten arm64 code); `tiers.sh` times every micro in toi and in V8
capped at each tier; `octane.sh` runs Octane (`SCORE=1` for scores);
`perf.sh --ab` compares two builds; `jit-asm.sh` and `stencils-asm.sh`
disassemble generated code and stencils; `sizes.sh --record` appends
sizes.

Environment flags: `TOI_NO_JIT`, `TOI_TIER=<back edges>`, `TOI_TIME`,
`TOI_JIT_TIME`, `TOI_EXITS` (exits, entries and runtime calls by count),
`TOI_JIT_DUMP`, `TOI_JIT_LOG`, `TOI_PERF_MAP` (names JIT code for `perf`),
`TOI_NO_QUICKEN`, `TOI_COUNT`, `TOI_CODE_SHIFT`; for the collector,
`TOI_GC_STATS` (each collection), `TOI_GC_NURSERY` (MiB),
`TOI_HEAP_LIMIT` (MiB), `TOI_GC_VERIFY=<KiB>` (collect that often,
checking references and poisoning what is freed), `TOI_GC_STRESS`
(collect at every safepoint), `TOI_GC_FULL` (every collection full),
`TOI_GC_STW` (full collections stop the world, no marker thread),
`TOI_GC_CYCLES` (major cycles back to back),
`TOI_GC_CHECK_BARRIER` (find missed barriers), `TOI_HEAP_HIST` (cells by
kind and size), `TOI_SEGV` (a backtrace on a crash). `gc-bench.sh` times
Octane's deterministic runs with their peak RSS.

## Building

Nightly Rust (`rust-toolchain.toml`). The JIT needs three unstable
features: `explicit_tail_calls`, `rust_preserve_none_cc` and
`asm_goto_with_outputs`.

- **Linux**, x86-64 and arm64, is the reference. Host builds are static
  and not position-independent (`.cargo/config.toml`), which is where the
  startup time comes from. arm64 builds cross-compile to musl and run
  under `qemu-aarch64`.
- **macOS on Apple silicon** builds and runs natively. The stencils come
  out byte for byte the same as Linux's: `build.rs` reads Mach-O, the
  holes that ELF expresses as `movz`/`movk` relocations (which Mach-O
  lacks) are written as magic immediates that `build.rs` turns back into
  those relocations, and the stencils compile for the `generic` CPU as on
  Linux (the Darwin default schedules for the host's). The one stencil
  that differs, `construct_direct`, swaps two independent instructions
  before a runtime call, because Darwin reserves x18 and LLVM allocates
  from one register fewer. Pages are 16 KiB there. Intel Macs are not
  supported (x86-64 Mach-O has no 32-bit absolute relocation for the
  immediate holes).

Because `build.rs` extracts stencils from LLVM's machine code and then
rewrites branches in it, the stencil pipeline depends on LLVM's codegen
choices. A nightly bump can change stencil shapes; the
`every_stencil_instance_matches_its_compiled_bytes` test and
`scripts/stencils-asm.sh` are the checks, and the generated
`stencils.rs` (in `target/*/build/toi-*/out/`) can be diffed between
targets.

## Where it falls short

- **The collector's minor pauses grow with the nursery's survivors**
  (up to 16 ms on TypeScript, 8 ms on Splay; major pauses stay under
  16 ms as a second thread marks and sweeps), Splay spends 19% of its
  time collecting, and it never moves a cell, so randomly dying cells
  fragment its blocks (Splay peaks at 657 MB for 120 MB live).
- **ES5 sloppy mode only.** No `let`, `const`, classes, arrow functions,
  template literals, strict mode or array holes. Correctness is checked
  against Node on about 100 programs and Octane, not against test262.
- **Nightly only**, and Linux is the reference; macOS runs on Apple silicon only.
- **Safety.** Around 560 `unsafe` sites, a raw-pointer object model, no
  fuzzing; a heap sandbox is deferred by design.
- **Compile-time churn** on large programs: TypeScript, pdfjs and
  code-load slowed when more of their functions compiled, from
  recompiling as feedback changes. A cheaper baseline tier is the
  planned fix.

## Layout

```text
src/js/          lexer, parser, AST, bytecode generation
src/bytecode.rs  the instruction set;  src/asm.rs  its text form
src/interp.rs    the interpreter;  src/vm.rs  runtime, builtins' host
src/object.rs    objects, shapes;  src/builtins.rs  the builtins
src/heap.rs      the heap;  src/gc.rs  the collector
src/jit.rs       the JIT;  stencils/stencils.rs  its stencils;  build.rs  extraction
src/regexp.rs    regular expressions;  src/math.rs  Math's transcendental functions (fdlibm)
tests/           differential tests;  tests/octane/  feature scripts per benchmark
bench/micro/     one pattern per file;  bench/bytecode/  handwritten bytecode
docs/            design log and designs
```
