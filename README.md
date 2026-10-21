# cloison

A JavaScript engine in Rust: a front end for ES5 and much of ES2015, a register bytecode
interpreter, and a copy-and-patch JIT for x86-64 and arm64 (Linux, and macOS on Apple silicon) that
grows into a mid-tier optimizing compiler without an IR beyond bytecode.

The whole engine is small: a 3 MB binary with 1.7 MB of machine code,
against 21 MB of code in a release build of V8's `d8` shell (12.8, arm64).
Yet on several Octane benchmarks it comes close to V8 14.6 capped at its
Maglev tier: Richards at 0.87 of Maglev's score, DeltaBlue at 0.81, Crypto
at 0.66 and NavierStokes at 0.57 (geomean 0.32 over all of Octane; see
[Results](#results)).

The name: in cloisonné enamel, *cloisons* are thin metal walls that
make fixed cells, which are then filled with enamel. The JIT's stencils
are the same idea, fixed machine code with holes filled in for each
instruction.

```sh
cargo build --release
target/release/cloison bench/micro/87-constraint-solver.js   # prints the completion value
target/release/cloison --dump file.js                         # the bytecode instead
target/release/cloison --module main.mjs                      # a module (a .mjs file is one anyway)
```

Status (2026-10-14): every Octane benchmark runs, with a generational
garbage collector that marks and sweeps its old generation concurrently
and frees compiled code that is no longer reachable. The language is
ES5 through ES2023 with gaps, four fifths of test262 (see
[Standards support](#standards-support)). See
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

## Design choices

- **Bytecode is the only IR.** The JIT compiles straight from the
  interpreter's bytecode, with types inferred over it; there is no SSA or
  sea-of-nodes graph to build, keep consistent or debug.
- **LLVM writes the machine code, ahead of time.** Copy-and-patch
  stencils are Rust compiled by LLVM at build time, so the JIT gets an
  optimizing backend's instruction selection without carrying one.
- **The interpreter is a tier worth having.** Tail-calling handlers with
  state in registers and an int32 handler table keep it near V8's
  Ignition, so code the JIT skips still runs well.
- **Speculate cheaply, exit freely.** Compiled code assumes the types
  and shapes it saw and side-exits to the interpreter when wrong, into
  the VM frames it keeps up to date (an inlined call's frame and a
  scalar-replaced object are materialized at the exit); tier-up is by
  on-stack replacement at loop back edges.
- **Features land in the interpreter first.** A function using an
  instruction the JIT doesn't know runs interpreted, so new language
  features (classes, destructuring, generators, async functions) cost
  the JIT nothing and can't slow hot ES5 code.
- **A simple, non-moving collector.** Conservative native roots let
  runtime Rust hold raw pointers without handles everywhere; a nursery,
  card marking and concurrent old-generation marking are built on top.
- **Bit-for-bit V8 numerics.** fdlibm `Math` and shortest round-trip
  number printing make results match Node exactly, which makes
  differential testing against Node possible.
- **Measured, not assumed.** Every change is checked against Octane and
  the microbenchmarks; code size is tracked per commit, and
  `docs/future-work.md` records what each step measured and what failed.
- **Small and self-contained.** One Rust crate whose only dependency is
  `libc`; a 3 MB binary that starts in 0.29 ms.

## Standards support

test262 at d53ebc8, every test under `built-ins`, `language` and
`annexB` (not `intl402` or `staging`), run as its `INTERPRETING.md`
says: in both strict and sloppy mode unless flagged, negative tests in
their phase, async tests to completion, module tests as modules (the
harness a script run first: `cloison --prelude harness.js --module test.js`).
A test counts toward the latest edition of its features; an
untagged test counts as ES5 (`scripts/test262-editions.py`):

| edition | passed | tests | ratio |
|---|---:|---:|---:|
| ES5 (untagged) | 16,794 | 17,067 | 98.4% |
| ES2015 | 8,937 | 9,197 | 97.2% |
| ES2016 | 97 | 100 | 97.0% |
| ES2017 | 699 | 741 | 94.3% |
| ES2018 | 4,735 | 4,825 | 98.1% |
| ES2019 | 159 | 161 | 98.8% |
| ES2020 | 1,979 | 2,020 | 98.0% |
| ES2021 | 617 | 630 | 97.9% |
| ES2022 | 5,170 | 5,374 | 96.2% |
| ES2023 | 284 | 284 | 100.0% |
| ES2024 | 840 | 843 | 99.6% |
| ES2025 | 980 | 998 | 98.2% |
| ES2026 | 745 | 745 | 100.0% |
| Stage 3 proposals | 5,625 | 5,650 | 99.6% |
| **all** | **47,661** | **48,635** | **98.0%** |

The `Atomics` tests that need several agents run them as test262's
`$262.agent` has them: each agent a thread with a VM of its own, sharing
`SharedArrayBuffer`s' memory with the main one (`CanBlockIsFalse` tests
run with `cloison --no-block`).

Temporal has the ISO 8601 calendar alone (as without ECMA-402), and
takes named time zones from the system's database (`TZDIR`, else
`/usr/share/zoneinfo`).

Realms (`ShadowRealm`, `$262.createRealm`) share the one heap: each
has its own global object and intrinsics, which a call of another
realm's function swaps in for the call (`src/realm.rs`). Until a second
realm is made, nothing checks; after, the JIT is off.

The gaps are mostly whole features: decorators, non-ASCII identifiers
beyond a rough approximation (no Unicode ID tables), and early errors
for invalid syntax that the parser accepts.

## Results

Octane scores at c842d29 on an Apple silicon Mac, cloison's JIT against V8 14.6
capped at Sparkplug, at Maglev, and with all tiers (higher is better;
from `perf-history/octane-engines.sh`):

| benchmark | V8 | V8 Maglev | cloison | V8 Sparkplug |
|---|---:|---:|---:|---:|
| Richards | 51,460 | 37,357 | 32,393 | 2,877 |
| DeltaBlue | 133,129 | 58,951 | 47,850 | 3,190 |
| Crypto | 60,295 | 33,389 | 22,164 | 2,887 |
| RayTrace | 141,412 | 89,095 | 45,094 | 8,806 |
| EarleyBoyer | 114,968 | 70,293 | 24,046 | 14,517 |
| RegExp | 13,883 | 13,456 | 4,373 | 7,509 |
| Splay | 102,198 | 100,162 | 17,215 | 18,651 |
| NavierStokes | 40,473 | 26,955 | 15,403 | 3,346 |
| PdfJS | 95,899 | 87,999 | 10,845 | 19,104 |
| Mandreel | 105,810 | 68,189 | 6,290 | 5,744 |
| GameBoy | 147,921 | 138,912 | 58,589 | 16,714 |
| CodeLoad | 74,977 | 75,635 | 32,425 | 75,696 |
| Box2D | 152,509 | 126,873 | 30,874 | 10,110 |
| zlib | 123,337 | 124,544 | 27,564 | 124,645 |
| TypeScript | 181,891 | 176,601 | 29,379 | 52,150 |
| **geomean** | **88,060** | **67,804** | **22,033** | **11,951** |

V8 runs zlib (asm.js) through its wasm pipeline, and compiles the
functions of CodeLoad's `eval`s when they are first called; cloison compiles
every function of every `eval` up front.

Against other engines and V8's tiers, on an Apple silicon Mac at c842d29
(higher is better; single runs from `perf-history/octane-engines.sh`,
cloison's JIT against V8 14.6 capped at each tier and with all of them, the
macOS 27 JavaScriptCore, SpiderMonkey 140 and QuickJS 2026-06-04; ARKTS
is OpenHarmony's ARK JS runtime, `ark_js_vm` at ets_runtime cf2d2a08f, each
benchmark compiled to `.abc` by `es2abc` at ets_frontend a7487a110, median of
three runs; on macOS its build leaves the asm interpreter out, so this is
its C++ interpreter, with no JIT or AOT):

| benchmark | JSC | V8 | V8 Maglev | SpiderMonkey | cloison | V8 Sparkplug | V8 Ignition | QuickJS | ARKTS | cloison / Maglev |
|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|
| Richards | 49,006 | 51,460 | 37,357 | 15,025 | 32,393 | 2,877 | 1,675 | 1,298 | 619 | 0.87 |
| DeltaBlue | 82,912 | 133,129 | 58,951 | 17,118 | 47,850 | 3,190 | 1,584 | 1,379 | 599 | 0.81 |
| Crypto | 76,263 | 60,295 | 33,389 | 26,215 | 22,164 | 2,887 | 1,665 | 1,315 | — | 0.66 |
| NavierStokes | 33,471 | 40,473 | 26,955 | 21,263 | 15,403 | 3,346 | 2,473 | 2,621 | — | 0.57 |
| RayTrace | 204,829 | 141,412 | 89,095 | 51,452 | 45,094 | 8,806 | 5,811 | 2,841 | 1,422 | 0.51 |
| CodeLoad | 82,525 | 74,977 | 75,635 | 45,252 | 32,425 | 75,696 | 75,863 | 21,234 | — | 0.43 |
| GameBoy | 168,246 | 147,921 | 138,912 | 102,991 | 58,589 | 16,714 | 11,591 | 11,341 | — | 0.42 |
| EarleyBoyer | 124,227 | 114,968 | 70,293 | 51,415 | 24,046 | 14,517 | 8,343 | 3,674 | — | 0.34 |
| RegExp | 22,616 | 13,883 | 13,456 | 11,875 | 4,373 | 7,509 | 5,581 | 491 | 624 | 0.32 |
| Box2D | 162,440 | 152,509 | 126,873 | 67,910 | 30,874 | 10,110 | 6,487 | 5,523 | 3,725 | 0.24 |
| zlib | 46,747 | 123,337 | 124,544 | 104,811 | 27,564 | 124,645 | 122,350 | 2,731 | — | 0.22 |
| Splay | 130,031 | 102,198 | 100,162 | 44,771 | 17,215 | 18,651 | 15,270 | 10,119 | 1,397 | 0.17 |
| TypeScript | 175,386 | 181,891 | 176,601 | 112,638 | 29,379 | 52,150 | 30,157 | 20,696 | — | 0.17 |
| PdfJS | 98,632 | 95,899 | 87,999 | 63,155 | 10,845 | 19,104 | 12,464 | 5,548 | — | 0.12 |
| Mandreel | 113,441 | 105,810 | 68,189 | 42,404 | 6,290 | 5,744 | 4,061 | 4,189 | — | 0.09 |
| **geomean** | **88,521** | **88,060** | **67,804** | **41,718** | **22,033** | **11,951** | **8,168** | **3,830** | — | **0.32** |

`es2abc` compiles every script as strict mode code, so Octane runs on ARK
only with its harness's `alert` declared (`var alert;` first) and not at
all where a benchmark relies on sloppy mode or what the runtime lacks:
EarleyBoyer (octal escapes), TypeScript (assigns `arguments`) and zlib
(`delete` of a variable) don't compile, Crypto and Mandreel assign
undeclared globals, NavierStokes and GameBoy fail on strict mode's
errors, PdfJS needs `console` and CodeLoad `eval` (unsupported). Hence no
geomean for it.

The numbers that follow are from an Ubuntu Linux x86-64 host.

Octane there at 957e562, cloison's JIT against V8 14.6 (Node 26) capped at
Maglev and at Sparkplug (medians of three runs, from
`scripts/octane-tiers.sh`, which also has cloison's interpreter and V8's other
tiers). At c842d29, above, a function calling `eval` directly dropped
every function's compiled code each time it ran; Mandreel (5,156 here
before the fix) and PdfJS (9,094) were the benchmarks it hit:

| benchmark | V8 Maglev | cloison | V8 Sparkplug | cloison / Maglev |
|---|---:|---:|---:|---:|
| DeltaBlue | 57,146 | 58,514 | 2,863 | 1.02 |
| Richards | 40,813 | 34,282 | 2,768 | 0.84 |
| Crypto | 36,112 | 21,635 | 2,477 | 0.60 |
| CodeLoad | 45,145 | 26,644 | 44,877 | 0.59 |
| Mandreel | 55,386 | 31,711 | 4,879 | 0.57 |
| NavierStokes | 38,063 | 21,200 | 3,121 | 0.56 |
| RayTrace | 83,101 | 38,997 | 7,370 | 0.47 |
| GameBoy | 119,967 | 55,879 | 14,378 | 0.47 |
| RegExp | 11,649 | 4,312 | 6,675 | 0.37 |
| EarleyBoyer | 63,367 | 22,606 | 13,590 | 0.36 |
| TypeScript | 82,688 | 28,379 | 34,377 | 0.34 |
| Splay | 31,343 | 10,130 | 7,167 | 0.32 |
| Box2D | 115,372 | 27,137 | 8,231 | 0.24 |
| zlib | 109,310 | 23,320 | 109,702 | 0.21 |
| PdfJS | 67,232 | 13,297 | 16,362 | 0.20 |
| **geomean** | **55,061** | **23,628** | **9,508** | **0.43** |

ARKTS there, where its build has the asm interpreter and its JIT, on the
six benchmarks it runs, at f2c093c (median of three runs; `ark_js_vm` at
ets_runtime ce9f9d11f, its asm interpreter and with `--compiler-enable-jit`
(arksteed), each benchmark compiled by `es2abc` at ets_frontend 333c4cf06,
with `var alert;` first as above; V8 is Node 24's, V8 13.6):

| benchmark | V8 | cloison | ARKTS JIT | ARKTS asm interpreter | cloison / ARKTS JIT |
|---|---:|---:|---:|---:|---:|
| Richards | 26,508 | 14,340 | 3,047 | 1,058 | 4.7 |
| DeltaBlue | 76,254 | 33,476 | 2,810 | 1,190 | 11.9 |
| RayTrace | 65,341 | 28,564 | 2,312 | 2,506 | 12.4 |
| RegExp | 6,558 | 2,962 | 450 | 429 | 6.6 |
| Splay | 37,981 | 8,069 | 6,356 | 5,532 | 1.3 |
| Box2D | 76,915 | 13,944 | 8,754 | 3,743 | 1.6 |
| **geomean** | **36,914** | **12,882** | **2,813** | **1,743** | **4.6** |

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
node bench/run.mjs --engine target/release/cloison        # every micro against `expect`
cargo test --target aarch64-unknown-linux-musl         # arm64 under qemu-aarch64
```

Scripts in `scripts/`: `stencil-listing.py` lists every stencil as the
JIT emits it, with instruction counts (`docs/arm64-targets.md` holds
them against handwritten arm64 code); `tiers.sh` times every micro in cloison and in V8
capped at each tier; `octane.sh` runs Octane (`SCORE=1` for scores);
`perf.sh --ab` compares two builds; `jit-asm.sh` and `stencils-asm.sh`
disassemble generated code and stencils; `sizes.sh --record` appends
sizes; `octane-sizes.sh` reports per Octane benchmark the bytecode, the
machine code and the compile time cloison spends, against V8, JavaScriptCore,
SpiderMonkey and QuickJS (installed with `jsvu`, measured once per
version: `bench/octane-sizes-engines.tsv`), with each engine's runtime
size; `--record` appends cloison's to `bench/octane-sizes.tsv`.
`octane-memory.sh` reports per Octane benchmark the peak and mean RSS of
cloison and of those engines (`bench/octane-memory-engines.tsv`), and
cloison's collections: pauses, the heap's peak against the most found
live; `--record` appends cloison's to `bench/octane-memory.tsv`.

Environment flags: `CLOISON_NO_JIT`, `CLOISON_TIER=<back edges>`, `CLOISON_TIME`,
`CLOISON_JIT_TIME`, `CLOISON_SIZES` (functions, bytecode, machine code and
compile time over the run; and the collections: pauses, heap peak, live peak), `CLOISON_EXITS` (exits, entries and runtime calls by count),
`CLOISON_JIT_DUMP`, `CLOISON_JIT_LOG`, `CLOISON_PERF_MAP` (names JIT code for `perf`),
`CLOISON_NO_QUICKEN`, `CLOISON_COUNT`, `CLOISON_CODE_SHIFT`; for the collector,
`CLOISON_GC_STATS` (each collection), `CLOISON_GC_NURSERY` (MiB),
`CLOISON_HEAP_LIMIT` (MiB), `CLOISON_GC_VERIFY=<KiB>` (collect that often,
checking references and poisoning what is freed), `CLOISON_GC_STRESS`
(collect at every safepoint), `CLOISON_GC_FULL` (every collection full),
`CLOISON_GC_STW` (full collections stop the world, no marker thread),
`CLOISON_GC_CYCLES` (major cycles back to back),
`CLOISON_GC_THREADS` (threads a pause scans cards and traces with; 1 for none),
`CLOISON_GC_CHECK_BARRIER` (find missed barriers), `CLOISON_HEAP_HIST` (cells by
kind and size), `CLOISON_SEGV` (a backtrace on a crash). `gc-bench.sh` times
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
choices, so `rust-toolchain.toml` pins the nightly, and a bump is a
deliberate change. The build checks every stencil's code for what the
JIT relies on (`build/verify.rs`: it decodes, its relocations are
branches' or operands' fields, its local branches stay inside, it
references no data, an exit comes before any store) and fails
otherwise. `scripts/stencil-snapshot.sh` compares the stencils of two
toolchains, `scripts/stencils-asm.sh` disassembles them, and the
generated `stencils.rs` (in `target/*/build/cloison-*/out/`) can be
diffed between targets.

## Where it falls short

- **The collector's pauses grow with the nursery's survivors**, though
  pauses trace and scan cards on several threads (up to 8 ms on
  TypeScript, 4.5 ms on Splay; a second thread marks and sweeps the old
  generation), Splay spends 12% of its time collecting, and it never
  moves a cell, so randomly dying cells fragment its blocks (Splay peaks
  at 657 MB for 120 MB live).
- **Language gaps.** Strict mode, `let`/`const`, classes, arrows,
  templates, destructuring, spread, symbols, iterators, generators,
  async functions and generators, `Reflect`, `Proxy`, BigInt, class
  fields, iterator helpers, `using` declarations and modules (top-level
  `await`, `import()`, JSON modules) are in; non-ASCII identifiers (no Unicode ID tables),
  array indices past 2^28 and Unicode case folding in regexps are not.
  See [Standards support](#standards-support) for test262 by edition;
  Octane and about 100 programs are also checked against Node.
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

## License

Licensed under the Apache License, Version 2.0; see `LICENSE`.
