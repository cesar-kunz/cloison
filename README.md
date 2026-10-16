# toi

A JavaScript engine in Rust: an ES5 front end, a register bytecode
interpreter, and a copy-and-patch JIT for x86-64 and arm64 (Linux, and macOS on Apple silicon) that
grows into a mid-tier optimizing compiler without an IR beyond bytecode.

```sh
cargo build --release
target/release/toi bench/micro/87-constraint-solver.js   # prints the completion value
target/release/toi --dump file.js                         # the bytecode instead
```

Status (2026-10-14): every Octane benchmark runs, with a generational
garbage collector that marks and sweeps its old generation concurrently
and frees compiled code that is no longer reachable. The language is
most of ES2017 short of modules, `Proxy` and BigInt. See
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

Octane scores at 49c80a1, toi's JIT against V8 14.6 with all tiers and
capped at Sparkplug (higher is better; medians of 3 runs, from
`scripts/octane-tiers.sh`):

| benchmark | toi | V8 Sparkplug | V8 |
|---|---:|---:|---:|
| Richards | 33,657 | 2,743 | 51,308 |
| DeltaBlue | 58,640 | 2,804 | 120,738 |
| Crypto | 20,221 | 2,442 | 50,339 |
| RayTrace | 22,303 | 7,348 | 132,902 |
| EarleyBoyer | 19,341 | 13,690 | 96,935 |
| RegExp | 5,513 | 6,668 | 11,851 |
| Splay | 10,767 | 7,283 | 35,231 |
| NavierStokes | 7,795 | 3,104 | 47,441 |
| PdfJS | 14,716 | 16,331 | 60,628 |
| Mandreel | 25,110 | 4,860 | 72,396 |
| GameBoy | 55,987 | 14,365 | 107,033 |
| CodeLoad | 18,374 | 45,023 | 45,102 |
| Box2D | 16,609 | 8,286 | 112,889 |
| zlib | 22,747 | 109,623 | 109,702 |
| TypeScript | 29,298 | 34,545 | 74,261 |
| **geomean** | **20,030** | **9,494** | **64,968** |

V8 runs zlib (asm.js) through its wasm pipeline, and compiles the
functions of CodeLoad's `eval`s when they are first called; toi compiles
every function of every `eval` up front.

Against other engines and V8's tiers, on an Apple silicon Mac at 242aafd
(higher is better; single runs from `perf-history/octane-engines.sh`,
toi's JIT against V8 14.6 capped at each tier and with all of them, the
macOS 27 JavaScriptCore, SpiderMonkey 140 and QuickJS 2026-06-04):

| benchmark | JSC | V8 | V8 Maglev | SpiderMonkey | toi | V8 Sparkplug | V8 Ignition | QuickJS | toi / Maglev |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Richards | 49,006 | 51,460 | 37,357 | 15,025 | 32,393 | 2,877 | 1,675 | 1,298 | 0.87 |
| DeltaBlue | 82,912 | 133,129 | 58,951 | 17,118 | 47,850 | 3,190 | 1,584 | 1,379 | 0.81 |
| Crypto | 76,263 | 60,295 | 33,389 | 26,215 | 22,164 | 2,887 | 1,665 | 1,315 | 0.66 |
| NavierStokes | 33,471 | 40,473 | 26,955 | 21,263 | 15,403 | 3,346 | 2,473 | 2,621 | 0.57 |
| RayTrace | 204,829 | 141,412 | 89,095 | 51,452 | 45,094 | 8,806 | 5,811 | 2,841 | 0.51 |
| CodeLoad | 82,525 | 74,977 | 75,635 | 45,252 | 32,425 | 75,696 | 75,863 | 21,234 | 0.43 |
| GameBoy | 168,246 | 147,921 | 138,912 | 102,991 | 58,589 | 16,714 | 11,591 | 11,341 | 0.42 |
| EarleyBoyer | 124,227 | 114,968 | 70,293 | 51,415 | 24,046 | 14,517 | 8,343 | 3,674 | 0.34 |
| RegExp | 22,616 | 13,883 | 13,456 | 11,875 | 4,373 | 7,509 | 5,581 | 491 | 0.32 |
| Box2D | 162,440 | 152,509 | 126,873 | 67,910 | 30,874 | 10,110 | 6,487 | 5,523 | 0.24 |
| zlib | 46,747 | 123,337 | 124,544 | 104,811 | 27,564 | 124,645 | 122,350 | 2,731 | 0.22 |
| Splay | 130,031 | 102,198 | 100,162 | 44,771 | 17,215 | 18,651 | 15,270 | 10,119 | 0.17 |
| TypeScript | 175,386 | 181,891 | 176,601 | 112,638 | 29,379 | 52,150 | 30,157 | 20,696 | 0.17 |
| PdfJS | 98,632 | 95,899 | 87,999 | 63,155 | 10,845 | 19,104 | 12,464 | 5,548 | 0.12 |
| Mandreel | 113,441 | 105,810 | 68,189 | 42,404 | 6,290 | 5,744 | 4,061 | 4,189 | 0.09 |
| **geomean** | **88,521** | **88,060** | **67,804** | **41,718** | **22,033** | **11,951** | **8,168** | **3,830** | **0.32** |

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
sizes; `octane-sizes.sh` reports per Octane benchmark the bytecode, the
machine code and the compile time toi spends, against V8, JavaScriptCore,
SpiderMonkey and QuickJS (installed with `jsvu`, measured once per
version: `bench/octane-sizes-engines.tsv`), with each engine's runtime
size; `--record` appends toi's to `bench/octane-sizes.tsv`.

Environment flags: `TOI_NO_JIT`, `TOI_TIER=<back edges>`, `TOI_TIME`,
`TOI_JIT_TIME`, `TOI_SIZES` (functions, bytecode, machine code and
compile time over the run), `TOI_EXITS` (exits, entries and runtime calls by count),
`TOI_JIT_DUMP`, `TOI_JIT_LOG`, `TOI_PERF_MAP` (names JIT code for `perf`),
`TOI_NO_QUICKEN`, `TOI_COUNT`, `TOI_CODE_SHIFT`; for the collector,
`TOI_GC_STATS` (each collection), `TOI_GC_NURSERY` (MiB),
`TOI_HEAP_LIMIT` (MiB), `TOI_GC_VERIFY=<KiB>` (collect that often,
checking references and poisoning what is freed), `TOI_GC_STRESS`
(collect at every safepoint), `TOI_GC_FULL` (every collection full),
`TOI_GC_STW` (full collections stop the world, no marker thread),
`TOI_GC_CYCLES` (major cycles back to back),
`TOI_GC_THREADS` (threads a pause scans cards and traces with; 1 for none),
`TOI_GC_CHECK_BARRIER` (find missed barriers), `TOI_HEAP_HIST` (cells by
kind and size), `TOI_SEGV` (a backtrace on a crash). `gc-bench.sh` times
Octane's deterministic runs with their peak RSS. `gc-micro.sh` runs
the collector's microbenchmarks in `bench/gc/`, which reproduce what it
still does badly.

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

- **The collector's pauses grow with the nursery's survivors**, though
  pauses trace and scan cards on several threads (up to 8 ms on
  TypeScript, 4.5 ms on Splay; a second thread marks and sweeps the old
  generation), Splay spends 12% of its time collecting, and it never
  moves a cell, so randomly dying cells fragment its blocks (Splay peaks
  at 657 MB for 120 MB live).
- **Language gaps.** Strict mode, `let`/`const`, classes, arrows,
  templates, destructuring, spread, symbols, iterators, generators,
  async functions and `Reflect` are in; modules, `Proxy`, BigInt, async
  generators, non-ASCII identifiers (no Unicode ID tables), array
  indices past 2^28 and Unicode case folding in regexps are not.
  test262's tests that need no feature flag (14,943 of them) pass at
  98.9%; Octane and about 100 programs are also checked against Node.
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
