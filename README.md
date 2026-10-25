# cloison

A JavaScript engine in Rust: a front end for ES5 through ES2026, a register bytecode
interpreter, and a copy-and-patch JIT for x86-64 and arm64 (Linux, and macOS on Apple silicon) that
grows into a mid-tier optimizing compiler without an IR beyond bytecode.

The whole engine is small: a 5.7 MB binary (stripped, static, x86-64) with
4.1 MB of machine code, against 21 MB of code in a release build of V8's
`d8` shell (12.8, arm64).
Yet on several Octane benchmarks it comes close to V8 14.6 capped at its
Maglev tier: Richards at 0.87 of Maglev's score, DeltaBlue at 0.81, Crypto
at 0.66 and NavierStokes at 0.57 on an Apple silicon Mac (geomean 0.32
over all of Octane there, 0.43 on Linux x86-64; see [Results](#results)).

The name: in cloisonné enamel, *cloisons* are thin metal walls that
make fixed cells, which are then filled with enamel. The JIT's stencils
are the same idea, fixed machine code with holes filled in for each
instruction.

```sh
cargo build --release
target/release/cloison bench/micro/87-constraint-solver.js   # prints the completion value
target/release/cloison --dump file.js                         # the bytecode instead
target/release/cloison --module main.mjs                      # a module (a .mjs file is one anyway)
target/release/cloison --array-density 1/16,1/8,8k,16m f.js   # when arrays go sparse (see below)
```

Status (2026-10-25): every Octane benchmark runs, with a generational
garbage collector that marks and sweeps its old generation concurrently
and frees compiled code that is no longer reachable. The language is
ES5 through ES2026 and most Stage 3 proposals, with gaps: 98.2% of
test262 (see [Standards support](#standards-support)). A differential
fuzzing campaign against V8, JavaScriptCore, SpiderMonkey and QuickJS has
found 28 bugs so far, all fixed. See
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
- **Arrays** keep their elements dense until too few are present for the
  room they take (`src/far.rs`): then the dense prefix stays and the rest,
  with the length, moves to a hash table, so `a = []; a[2 ** 31] = 1`
  peaks at 4.6 MB of RSS (an empty script at 3.8) and indices up to
  2^32 − 2 work; array methods skip what isn't there. The trade-off
  between speed and memory (how sparse, how much waste) was chosen by a
  sweep over nine workloads (`bench/sparse/`) and can be set at run time
  or when building (`CLOISON_ARRAY_DENSE_MIN`,
  `CLOISON_ARRAY_DENSE_BACK`, `CLOISON_ARRAY_WASTE`,
  `CLOISON_ARRAY_PREALLOC`, or `--array-density`).
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

The commit messages are the design log: each step with what it took and
what it measured. `docs/gc.md` designs the collector (non-moving and
generational, cells in 16-byte granules with a metadata byte each,
conservative native roots, a card-marking barrier, concurrent marking of
the old generation) and tracks its implementation, `docs/object-model.md`
the object model and its escape analysis, `docs/sparse-arrays.md` sparse
arrays and how their defaults were chosen, `docs/direct-calls.md` the
call convention, `docs/arm64-targets.md` the stencils against handwritten
arm64 code, `docs/octane-tiers.md` Octane in cloison's and V8's tiers over
time, and `docs/sandbox.md` surveys heap sandboxing and defers it.

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
  the microbenchmarks, and its commit message records what it measured;
  code size (`bench/sizes.tsv`), Octane's memory (`bench/octane-memory.tsv`)
  and its code sizes (`bench/octane-sizes.tsv`) are tracked per commit.
- **Small and self-contained.** One Rust crate whose only dependency is
  `libc`; a static 5.7 MB binary whose empty script runs in about 1 ms
  more than `/bin/true` (Linux x86-64).

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
| ES5 (untagged) | 16,823 | 17,067 | 98.6% |
| ES2015 | 8,982 | 9,197 | 97.7% |
| ES2016 | 100 | 100 | 100.0% |
| ES2017 | 699 | 741 | 94.3% |
| ES2018 | 4,759 | 4,825 | 98.6% |
| ES2019 | 161 | 161 | 100.0% |
| ES2020 | 1,979 | 2,020 | 98.0% |
| ES2021 | 617 | 630 | 97.9% |
| ES2022 | 5,170 | 5,374 | 96.2% |
| ES2023 | 284 | 284 | 100.0% |
| ES2024 | 840 | 843 | 99.6% |
| ES2025 | 980 | 998 | 98.2% |
| ES2026 | 745 | 745 | 100.0% |
| Stage 3 proposals | 5,628 | 5,650 | 99.6% |
| **all** | **47,767** | **48,635** | **98.2%** |

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

The gaps are mostly whole features and early errors: decorators,
non-ASCII identifiers beyond an approximation (Rust's `is_alphabetic`, not
Unicode's ID_Start and ID_Continue), early errors for invalid syntax that
the parser accepts (most of the failures left), `super` and private names
in a direct `eval` in a class field's initializer, and part of Unicode's
case folding in regexps (`/\u0390/ui` doesn't match `\u1fd3`).

## Results

Octane scores at c842d29 on an Apple silicon Mac, cloison's JIT against V8 14.6
capped at Sparkplug, at Maglev, and with all tiers (higher is better;
single runs from a local script, `perf-history/octane-engines.sh`, which the
repository ignores):

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
(higher is better; single runs from the same local script,
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
binary's machine code is about 4.1 MB, stencil code about 170 KB (x86-64,
at 52f4c0e).

### Against other engines, besides peak speed

On the same Linux host (an AMD EPYC 9454P, Zen 4) at 56dc4da, against
the jsvu builds of V8 15.8 (`d8`), JavaScriptCore 323696 (the GTK port),
SpiderMonkey 159 and QuickJS-ng 0.17, each also without its JIT (V8
`--jitless`, JavaScriptCore `--useJIT=false`, SpiderMonkey `--no-blinterp
--no-baseline --no-ion --no-native-regexp`, cloison `CLOISON_NO_JIT`).
`scripts/compare/` measures it and keeps the results in `bench/compare/`
(`report.py` makes the tables, each with every engine's ratio to the best and
its rank). The engines' binaries run directly, not through jsvu's shell
wrappers, whose shell costs about a millisecond and 2 million instructions.
Octane is a single run of each `-score.js`, and the deterministic `-run.js`
(the same fixed work for every engine) under `perf stat`, one run at a time,
the host at a load of 3 to 4. Instruction counts don't vary with load, but
they don't weigh a cache miss as time does. Geomeans are over the 15
benchmarks (lower is better but for scores):

| | cloison | V8 | JSC | SpiderMonkey | QuickJS |
|---|---:|---:|---:|---:|---:|
| empty script: wall ms | 1.9 | 12.3 | 16.2 | 11.1 | 1.4 |
| empty script: million instructions | 6.7 | 30.2 | 79.1 | 49.5 | 3.8 |
| empty script: RSS MB | 3.8 | 16.5 | 31.5 | 19.5 | 3.6 |
| binary, stripped, MB | 5.7 | 35.6 | 36.8 | 44.7 | 1.8 |
| other files it needs | — | 10.8 MB ICU data | 39 MB ICU libraries | 3 NSPR libraries | — |
| shared libraries loaded | 0 (static) | 4 | 21 | 9 | 0 (static) |
| Octane, JIT | 27,476 | 75,961 | 72,924 | 47,127 | — |
| Octane, no JIT | **6,397** | 5,521 | 4,285 | 2,206 | 1,874 |
| fixed work, JIT: G instructions | 29.9 | 12.6 | 12.0 | 15.6 | — |
| fixed work, JIT: CPU s | 2.23 | 1.29 | 1.30 | 1.42 | — |
| fixed work, no JIT: G instructions | **104** | 157 | 151 | 361 | 315 |
| peak RSS, JIT: MB | 78.8 | 68.9 | 216 | 83.3 | — |
| peak RSS, no JIT: MB | 57.8 | 55.8 | 107 | 76.5 | 39.4 |

The binaries' sizes are what each engine needs to run: JavaScriptCore's is
its 0.5 MB shell and its 36.3 MB library, SpiderMonkey carries ICU in its
18.5 MB of read-only data. cloison and QuickJS have no `Intl`, much of the
difference. Startup and an empty script's memory put cloison next to
QuickJS, 6 to 8 times faster and 4 to 8 times smaller than the others.

Without a JIT, cloison's interpreter is the fastest of the five, first on
11 of the 15 benchmarks; V8's Ignition is ahead on RegExp, PdfJS and CodeLoad
(cloison compiles all of an `eval`'s functions up front), JavaScriptCore's
LLInt on Splay:

| benchmark | cloison | V8 Ignition | JSC LLInt | SpiderMonkey | QuickJS |
|---|---:|---:|---:|---:|---:|
| Richards | 2,725 | 1,966 | 1,860 | 482 | 748 |
| DeltaBlue | 2,565 | 1,953 | 1,163 | 491 | 765 |
| Crypto | 2,110 | 1,420 | 1,793 | 820 | 444 |
| RayTrace | 6,720 | 5,465 | 2,804 | 1,488 | 1,244 |
| EarleyBoyer | 8,325 | 8,134 | 4,670 | 1,999 | 2,156 |
| RegExp | 3,587 | 3,833 | 752 | 923 | 338 |
| Splay | 10,613 | 9,794 | 12,313 | 6,524 | 5,966 |
| NavierStokes | 3,558 | 2,179 | 2,743 | 1,413 | 1,265 |
| PdfJS | 10,246 | 10,792 | 8,737 | 5,233 | 3,066 |
| Mandreel | 4,051 | 3,333 | 2,637 | 1,133 | 1,115 |
| GameBoy | 12,354 | 9,683 | 8,372 | 3,661 | 4,073 |
| CodeLoad | 32,524 | 51,407 | 44,468 | 48,917 | 19,747 |
| Box2D | 8,068 | 5,859 | 6,588 | 2,320 | 3,291 |
| zlib | 2,987 | 2,423 | 2,287 | 1,428 | 662 |
| TypeScript | 27,344 | 26,764 | 18,088 | 8,261 | 11,821 |
| **geomean** | **6,397** | **5,521** | **4,285** | **2,206** | **1,874** |

With the JIT, cloison runs 2.5 times the instructions of JavaScriptCore for
the same work and takes 1.7 times its CPU time; its memory is second to V8's
(JavaScriptCore's heap is about 3 times the others'). Without the JIT, Splay
peaks at 1.85 GB of RSS (673 MB with it): its full collections find 1.7
million payload trees live where the compiled run's find 256 thousand,
dead nodes some interpreted state still reaches (not yet found).

The collectors, from each engine's own trace of the fixed work (cloison's
`CLOISON_SIZES` and `CLOISON_GC_STATS`, V8's `--trace-gc-nvp`,
SpiderMonkey's `JS_GC_PROFILE_NURSERY` and `JS_GC_PROFILE`, JavaScriptCore's
`--logGC=1`; QuickJS counts references, with no generations):

| | cloison | V8 | JSC | SpiderMonkey |
|---|---:|---:|---:|---:|
| bytes allocated, MB (geomean) | 567 | 155 | 333 | — |
| allocated young, % (geomean) | 100 | 73 | 100 | — |
| young bytes promoted, % (geomean) | 5.1 | 1.5 | 0.9 | 3.9 |
| minor collections (all 15) | 1,411 | 2,797 | 69 | 4,702 |
| major collections (all 15) | 42 | 9 | 3 | 31 |
| minor pauses, ms (all 15) | 1,268 | 467 | 126 | 378 |
| major pauses, ms (all 15) | 182 | 23 | 11 | 119 |
| longest minor pause, ms | 8.3 | 19.1 | 7.5 | 10.5 |
| longest major pause, ms | 9.3 | 5.4 | 4.4 | 5.0 |

Every cell cloison or JavaScriptCore allocates starts young; V8 allocates
some straight in its old generation (Splay 92% of its 626 MB, once its
allocation sites show they survive). cloison allocates 3.7 times V8's bytes:
Splay 4.1 GB, 70% of it promoted. Its major figures count both pauses of a
concurrent cycle, V8's only the final pause of an incremental marking (not
its steps), JavaScriptCore's every increment. SpiderMonkey's trace has only
its nursery's size and promotion rate: its allocation outside the nursery
is unknown, its young bytes a bound (the nursery's size at each
collection). JavaScriptCore's promotion is what an Eden collection leaves
above the heap the last one left (it moves nothing); its large heap makes
few collections.

## Testing

The differential tests run each program in four engine configurations
(the interpreter alone, and tiered up after 1, 2 and 1,000 loop back
edges) and the results must agree: the JavaScript tests (`tests/js.rs`,
294, which also verify the heap after each run), the bytecode ones
(`tests/objects.rs`), the 112 programs of `bench/micro/` and the 33 of
`tests/octane/`, which carry Node's value as the reference
(`tests/interp.rs` tests the interpreter alone). Several tests are
mutation checks that fail when a specific conversion or undo piece is
removed. test262 runs with `scripts/test262-editions.py` (a
checkout in `test262/`), which writes the table above.

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
`octane-tiers.sh` scores Octane in cloison's interpreter and JIT and in
each of V8's tiers (`docs/octane-tiers.md`); `quiet.sh` runs a command
pinned to the idlest core; `vs-v8.sh` times every micro against Node and
`vs-maglev.sh` compares the machine code with Maglev's, function by
function; `jit-profile.sh` profiles compiled code per stencil piece;
`dispatch-asm.sh` disassembles the interpreter's handlers.
`bench/sparse/sweep.sh` and `pareto.py` choose the sparse-array settings
(`bench/sparse/README.md`). `scripts/compare/` compares cloison with the
jsvu engines, with and without their JITs: `startup.py` (an empty script's
time, instructions and memory), `octane.py` (scores, and the fixed work's
instructions, CPU time and peak RSS), `gcgen.py` (generations and pauses
from each collector's trace), `m3.py` (bytes per object of each kind, from
the RSS `probe.py` samples after full collections) and `report.py` (the
tables, ratios and ranks), results in `bench/compare/`.

Environment flags: `CLOISON_NO_JIT` (the interpreter alone),
`CLOISON_NO_RX_JIT` (regexps interpreted), `CLOISON_WARM` (compile warm
first), `CLOISON_TIER=<back edges>`, `CLOISON_TIME`,
`CLOISON_JIT_TIME`, `CLOISON_SIZES` (functions, bytecode, machine code and
compile time over the run; the collections: pauses, heap peak, live peak; and what the JIT keeps, by kind), `CLOISON_EXITS` (exits, entries and runtime calls by count),
`CLOISON_JIT_DUMP` (`CLOISON_JIT_DUMP_FN=<name>` for one function),
`CLOISON_JIT_LOG`, `CLOISON_KEYED` (keyed loads' shapes as compiled),
`CLOISON_PERF_MAP` (names JIT code for `perf`),
`CLOISON_NO_QUICKEN`, `CLOISON_COUNT`, `CLOISON_CODE_SHIFT`; for the collector,
`CLOISON_GC_STATS` (each collection), `CLOISON_GC_FRAG` (blocks by occupancy and what could not move, after stop-the-world full collections), `CLOISON_GC_NURSERY` (MiB, fixed; else 8 to 32, grown on minor collections' share of the time),
`CLOISON_HEAP_LIMIT` (MiB), `CLOISON_GC_MIN_HOLE` (bytes; smaller holes wait
for the next collection), `CLOISON_GC_ROOTS` (what each kind of root
reaches), `CLOISON_GC_VERIFY=<KiB>` (collect that often,
checking references and poisoning what is freed), `CLOISON_GC_STRESS`
(collect at every safepoint), `CLOISON_GC_FULL` (every collection full),
`CLOISON_GC_STW` (full collections stop the world, no marker thread),
`CLOISON_GC_CYCLES` (major cycles back to back),
`CLOISON_GC_THREADS` (threads a pause scans cards and traces with; 1 for none),
`CLOISON_GC_CHECK_BARRIER` (find missed barriers), `CLOISON_HEAP_HIST` (cells by
kind and size), `CLOISON_SEGV` (a backtrace on a crash); for arrays,
`CLOISON_ARRAY_DENSE_MIN`, `CLOISON_ARRAY_DENSE_BACK`, `CLOISON_ARRAY_WASTE`
and `CLOISON_ARRAY_PREALLOC` (also read when building: see
`docs/sparse-arrays.md`). `gc-bench.sh` times
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
  TypeScript and on Splay; a second thread marks and sweeps the old
  generation), Splay spends 18% of its time in pauses, and it never
  moves a cell, so randomly dying cells fragment its blocks (Splay peaks
  at about 670 MB of RSS for 160 to 180 MB live). Measured on the Linux
  host at 52f4c0e, deterministic runs.
- **Language gaps.** Strict mode, `let`/`const`, classes, arrows,
  templates, destructuring, spread, symbols, iterators, generators,
  async functions and generators, `Reflect`, `Proxy`, BigInt, class
  fields, iterator helpers, `using` declarations and modules (top-level
  `await`, `import()`, JSON modules) and arrays of any length up to
  2^32 − 1, sparse ones too, are in; decorators, Unicode's identifier tables and part of its
  case folding in regexps are not.
  See [Standards support](#standards-support) for test262 by edition;
  Octane and about 145 programs are also checked against Node.
- **Nightly only**, and Linux is the reference; macOS runs on Apple silicon only.
- **Safety.** Around 1,200 `unsafe` blocks and functions, a raw-pointer
  object model, and no heap sandbox (deferred by design). It is fuzzed
  differentially (against V8, JavaScriptCore, SpiderMonkey and QuickJS,
  across its own tiers and collector settings): 28 bugs so far, each fixed
  with a regression test (wrong results, crashes, a collector bug that
  gave objects a dead shape).
- **Compile-time churn** on large programs: TypeScript, pdfjs and
  code-load slowed when more of their functions compiled, from
  recompiling as feedback changes. A cheaper baseline tier is the
  planned fix.

## Layout

```text
src/js/          lexer, parser, AST, bytecode generation
src/bytecode.rs  the instruction set;  src/asm.rs  its text form
src/interp.rs    the interpreter;  src/vm.rs  runtime, builtins' host
src/object.rs    objects, shapes;  src/props.rs  properties;  src/builtins.rs  the builtins
src/array.rs     Array's methods;  src/far.rs  sparse arrays' far store and density
src/heap.rs      the heap;  src/gc.rs  the collector
src/jit.rs       the JIT;  stencils/stencils.rs  its stencils;  build.rs  extraction
src/regexp.rs    regular expressions;  src/math.rs  Math's transcendental functions (fdlibm)
src/promise.rs   Promise;  src/generator.rs  generators;  src/module.rs  modules
src/iterator.rs  iterator helpers;  src/collections.rs  Map, Set, weak ones;  src/dispose.rs  `using`
src/typed.rs     ArrayBuffer, typed arrays;  src/shared.rs  SharedArrayBuffer, Atomics
src/proxy.rs, src/reflect.rs, src/bigint.rs, src/date.rs, src/temporal/  as named
src/realm.rs     realms;  src/shadow.rs  ShadowRealm;  src/agent.rs  test262's agents
tests/           differential tests;  tests/octane/  feature scripts per benchmark
bench/micro/     one pattern per file;  bench/bytecode/  handwritten bytecode
bench/gc/        the collector's microbenchmarks;  bench/sparse/  sparse-array workloads, sweep
bench/compare/   against other engines (scripts/compare/)
docs/            designs (the design log is the commit messages)
```

## License

Licensed under the Apache License, Version 2.0; see `LICENSE`.
