# cloison in depth

The long companion of [the README](../README.md): how the engine works and why,
what it supports, how it is tested and built, and where it falls short.
Benchmark reports are in [docs/bench/](bench/).

The name: in cloisonné enamel, *cloisons* are thin metal walls that
make fixed cells, which are then filled with enamel. The JIT's stencils
are the same idea, fixed machine code with holes filled in for each
instruction.

## How it works

- **Values** are NaN-boxed into 64 bits with JavaScriptCore's encoding
  (`src/value.rs`): pointers as is, int32s under a tag, doubles offset by
  2^49.
- **The interpreter** (`src/interp.rs`) is a tail-calling dispatcher: all
  state lives in `preserve_none` arguments, so in registers, and each
  handler ends in `become` to the next. A second handler table encodes
  "the accumulator is an int32" so hot arithmetic skips type checks.
  Comparison-and-jump pairs, `Star r; Ldar s`, a statement's `r++`,
  `GetNamed` chains and 21 more hot pairs (`PAIRS` in `src/bytecode.rs`)
  are quickened into superinstructions.
- **Objects** (`src/object.rs`, `src/vm.rs`) have shapes in transition
  trees, in-object slots, inline caches that record field types and
  element kinds, and a global prototype epoch that invalidates cached
  prototype-chain loads. Closures use Lua-style upvalues.
- **Arrays** keep their elements dense until too few are present for the
  room they take (`src/far.rs`): then the dense prefix stays and the rest,
  with the length, moves to a hash table, so `a = []; a[2 ** 31] = 1`
  peaks at 4.6 MB of RSS, as an empty script does, and indices up to
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
    that side-exits on overflow and widens, sums read only through
    ToInt32 that wrap instead, and `r += x` on an int32 slot in place;
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
    functions, guards and constant loads hoisted out of loops, field
    loads reused across joins, and jumps on a known boolean threaded
    past its test;
  - a function's compilations are capped by its size (`max_compiles`),
    and so are the type-keyed versions of an entry before it compiles
    for any types (`max_versions`: 4 under 400 instructions, 2 under
    1,600, then 1);
  - register fields are patched while copying, so a stencil's per-slot
    variants are one template (on x86-64, one per REX class): x86-64's
    3,969 stencils (174 KB of code) are 1,656 table entries (84 KB),
    each template checked byte for byte against LLVM's own compile of
    every variant it stands for.

- **Numbers print and compute as in V8.** `Math`'s transcendental
  functions (`src/math.rs`) are a port of fdlibm, which V8 ships, rather
  than the platform's `libm`, whose last bits differ between systems
  (Apple's `tan(1)` against glibc's): `sin`, `cos`, `tan`, `asin`,
  `acos`, `atan`, `atan2`, `exp` and `log` are bit for bit Node's
  on x86-64, checked on 57,000 inputs over every reduction path. Node's
  arm64 builds let the C++ compiler fuse multiply-adds in the same code,
  so about 0.5% of their results differ by one ulp; the tests' expected
  values come from x86-64. `pow` is the exception: fdlibm's has the last
  bit wrong on one input in ten (`Math.pow(2, -0.5)` isn't
  `Math.SQRT1_2`) and V8 has left it for the platform's, so cloison's is
  a port of Arm Optimized Routines' instead, which glibc and musl use
  too; it differs from today's V8 on Linux by one ulp on about one
  input in a thousand. Number to string picks the closest of the
  shortest round-tripping digit strings, ties to even, as ECMA-262 says
  and Rust's own formatting doesn't; checked on 22,000 doubles against
  Node.

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
- **Features land in the interpreter first.** Compiled code exits to
  the interpreter at an instruction the JIT doesn't know (generators and
  async functions run interpreted), so new language features (classes,
  destructuring, generators, async functions) cost the JIT nothing and
  can't slow hot ES5 code.
- **A mostly non-moving collector.** Conservative native roots let
  runtime Rust hold raw pointers without handles everywhere; a nursery,
  card marking and concurrent old-generation marking are built on top,
  and only the cells nothing pins move out of fragmented blocks
  (`docs/gc.md`).
- **Bit-for-bit V8 numerics.** fdlibm `Math` and shortest round-trip
  number printing make results match Node exactly, which makes
  differential testing against Node possible.
- **Measured, not assumed.** Every change is checked against Octane and
  the microbenchmarks, and its commit message records what it measured;
  code size (`bench/sizes.tsv`), Octane's memory (`bench/octane-memory.tsv`)
  and its code sizes (`bench/octane-sizes.tsv`) are tracked per commit.
- **Small and self-contained.** One Rust crate whose only runtime
  dependency is `libc` (ICU4X too with the optional `intl` feature;
  `build.rs` reads the stencils with `object` and decodes them with
  `iced-x86`); one static binary, no data files.

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
| ES5 (untagged) | 16,851 | 17,067 | 98.7% |
| ES2015 | 9,013 | 9,197 | 98.0% |
| ES2016 | 100 | 100 | 100.0% |
| ES2017 | 721 | 741 | 97.3% |
| ES2018 | 4,685 | 4,825 | 97.1% |
| ES2019 | 161 | 161 | 100.0% |
| ES2020 | 1,996 | 2,020 | 98.8% |
| ES2021 | 619 | 630 | 98.3% |
| ES2022 | 5,204 | 5,374 | 96.8% |
| ES2023 | 284 | 284 | 100.0% |
| ES2024 | 840 | 843 | 99.6% |
| ES2025 | 996 | 998 | 99.8% |
| ES2026 | 745 | 745 | 100.0% |
| Stage 3 proposals | 5,628 | 5,650 | 99.6% |
| **all** | **47,843** | **48,635** | **98.4%** |

Most of ES2018's failures (90) are property escapes' tests of code points
Unicode 18 assigns: cloison's tables are Unicode 18's, this test262's
Unicode 17's.

The `Atomics` tests that need several agents run them as test262's
`$262.agent` has them: each agent a thread with a VM of its own, sharing
`SharedArrayBuffer`s' memory with the main one (`CanBlockIsFalse` tests
run with `cloison --no-block`).

Temporal has the ISO 8601 calendar alone (as without ECMA-402), and
takes named time zones from the system's database (`TZDIR`, else
`/usr/share/zoneinfo`).

`Intl` (ECMA-402) is an optional feature, `cargo build --features
intl`: `Intl.Collator` alone so far, on ICU4X, its locale data (English)
a file read at runtime (`CLOISON_INTL_DATA`). See [docs/intl.md](intl.md).
`String.prototype.normalize` has all four forms without it, from tables
generated from Unicode 18 (`scripts/unicode-norm.py`).

Realms (`ShadowRealm`, `$262.createRealm`) share the one heap: each
has its own global object and intrinsics, which a call of another
realm's function swaps in for the call (`src/realm.rs`). Until a second
realm is made, nothing checks; after, the JIT is off.

The gaps are mostly whole features and early errors: decorators,
non-ASCII identifiers beyond an approximation (Rust's `is_alphabetic`, not
Unicode's ID_Start and ID_Continue), early errors for invalid syntax that
the parser accepts (most of the failures left), and private names in a
direct `eval`.

## Testing

The differential tests run each program in four engine configurations
(the interpreter alone, and tiered up after 1, 2 and 1,000 loop back
edges) and the results must agree: the JavaScript tests (the scripts of
`tests/scripts/`, each with its expected result in a leading comment, and
`tests/js.rs` for those that use the embedding API; both verify the heap
after each run), the bytecode ones
(`tests/objects.rs`), the programs of `bench/micro/` and of
`tests/octane/`, which carry Node's value as the reference
(`tests/interp.rs` tests the interpreter alone). Several tests are
mutation checks that fail when a specific conversion or undo piece is
removed. test262 runs with `scripts/test262-editions.py` (a
checkout in `test262/`), which writes the table above;
`scripts/test262-check.py` compares its failures with those known
(`tests/test262-known-fails.txt`, which names the test262 commit).

GitHub Actions runs the tests on x86-64, `cargo fmt --check` and
`cargo clippy --all-targets -- -D warnings` (also with `intl`) for every
pull request and push to master (`.github/workflows/ci.yml`), and nightly the tests on arm64
and with the `intl` feature, and test262 against the failures known
(`nightly.yml`, also run by hand from the Actions tab).

```sh
cargo test --release
node bench/run.mjs --engine target/release/cloison        # every micro against `expect`
cargo test --release --target aarch64-unknown-linux-musl   # arm64 under qemu-aarch64
scripts/tsan.sh                                            # under ThreadSanitizer
```

`scripts/tsan.sh` runs tests (the unit tests, `tests/heaps.rs` and
`tests/js.rs` and `tests/scripts/` by default, or cargo test's arguments) under
ThreadSanitizer on x86-64 Linux, with `-Zbuild-std` (`rustup component
add rust-src`): it finds races between the mutator, the concurrent
marker and agents in the Rust code, not in compiled code or asm, which
TSan doesn't see (the JIT is
off but where a test sets the tier, as the JavaScript tests do). `CYCLES=1`
adds `CLOISON_GC_CYCLES`. Such a build moves the heap's reservation and
side tables below 2 TiB, out of TSan's shadow memory, with room for 128
heaps of 8 GiB (`cloison_tsan` in `build.rs`); `scripts/tsan.supp` lists
the reports known benign, each with why.

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
`dispatch-asm.sh` disassembles the interpreter's handlers;
`stencil-targets.py` counts each hot stencil family's instructions
against the handwritten target in its `// target` comment (`--check`
fails on a change).
`bench/sparse/sweep.sh` and `pareto.py` choose the sparse-array settings
(`bench/sparse/README.md`). `scripts/compare/` compares cloison with the
jsvu engines, with and without their JITs: `startup.py` (an empty script's
time, instructions and memory), `octane.py` (scores, and the fixed work's
instructions, CPU time and peak RSS), `gcgen.py` (generations and pauses
from each collector's trace), `m3.py` (bytes per object of each kind, from
the RSS `probe.py` samples after full collections), `compile.py` (loading,
JIT compile time, deoptimizations), `probes.py` (memory given back, limits,
realms), `webapps.py` (real web apps' bundles: time, peak RSS and
whether they did the reference's work), `report.py` (the tables, ratios and ranks) and
`chart.py` (the README's chart, from `octane.tsv` and `startup.tsv`), results in
`bench/compare/`.

Environment flags: `CLOISON_NO_JIT` (the interpreter alone),
`CLOISON_NO_RX_JIT` (regexps interpreted), `CLOISON_WARM` (compile warm
first), `CLOISON_TIER=<back edges>`, `CLOISON_TIME`,
`CLOISON_JIT_TIME`, `CLOISON_SIZES` (functions, bytecode, machine code and
compile time over the run; the collections: pauses, heap peak, live peak; and what the JIT keeps, by kind), `CLOISON_EXITS` (exits, entries and runtime calls by count),
`CLOISON_JIT_DUMP` (`CLOISON_JIT_DUMP_FN=<name>` for one function),
`CLOISON_JIT_LOG`, `CLOISON_KEYED` (keyed loads' shapes as compiled),
`CLOISON_JIT_COMPILES` (a function's compilations, 32),
`CLOISON_JIT_COMPILE_SIZE` (instructions past which it gets fewer, and
its entries fewer versions, 400), `CLOISON_JIT_VERSIONS` (versions an
entry keeps before compiling for any types, 4) and
`CLOISON_JIT_RESERVE_CALLS` (calls a spent function's start waits for its
last compilation, 1,000), `CLOISON_JIT_REGRANTS` (compilations more for a
start refused for good, 1) and `CLOISON_JIT_REGRANT_CALLS` (the calls each
waits for, 10,000), also read when building,
`CLOISON_PERF_MAP` (names JIT code for `perf`),
`CLOISON_NO_QUICKEN`, `CLOISON_COUNT`, `CLOISON_CODE_SHIFT`; for the collector,
`CLOISON_GC_STATS` (each collection), `CLOISON_GC_FRAG` (blocks by occupancy and what could not move, after stop-the-world full collections), `CLOISON_GC_NURSERY` (MiB, fixed; else 8 to 32, grown on minor collections' share of the time),
`CLOISON_HEAP_LIMIT` (MiB), `CLOISON_GC_MIN_HOLE` (bytes; smaller holes wait
for the next collection), `CLOISON_GC_ROOTS` (what each kind of root
reaches), `CLOISON_GC_VERIFY=<KiB>` (collect that often,
checking references and poisoning what is freed), `CLOISON_GC_STRESS`
(collect at every safepoint), `CLOISON_GC_FULL` (every collection full),
`CLOISON_GC_STW` (full collections stop the world, no marker thread),
`CLOISON_GC_EVACUATE` (`0` none, `all` the moving stress mode; the
policy's knobs are `CLOISON_GC_EVACUATE_*`, see `gc::Policy`),
`CLOISON_GC_SLOTS` (check that slots reach every marked cell and none
refers to a moved one),
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
  pauses trace and scan cards on several threads, and the heap's blocks
  stay fragmented where cells die at random faster than evacuation runs:
  Splay's cycles come too close together to evacuate at all
  (`docs/gc.md`, plan step 7). Objects are 2 to 4 times V8's bytes
  ([docs/bench/memory.md](bench/memory.md)).
- **Language gaps.** Strict mode, `let`/`const`, classes, arrows,
  templates, destructuring, spread, symbols, iterators, generators,
  async functions and generators, `Reflect`, `Proxy`, BigInt, class
  fields, iterator helpers, `using` declarations and modules (top-level
  `await`, `import()`, JSON modules) and arrays of any length up to
  2^32 − 1, sparse ones too, are in; decorators and Unicode's identifier
  tables are not.
  See [Standards support](#standards-support) for test262 by edition;
  Octane and the microbenchmarks are also checked against Node.
- **Nightly only**, and Linux is the reference; macOS runs on Apple silicon only.
- **Safety.** Many `unsafe` blocks, a raw-pointer object model, and no
  heap sandbox (deferred by design, `docs/sandbox.md`). It is fuzzed
  differentially (against V8, JavaScriptCore, SpiderMonkey and QuickJS,
  across its own tiers and collector settings), each bug fixed with a
  regression test.
- **Large programs recompile as their feedback changes**, and a function
  that has used up its compilations (and the one more it gets 10,000
  calls later) runs interpreted from then on, however hot it gets.
  Loading a script (parsing it and compiling to bytecode) is several
  times slower than in V8 or JavaScriptCore.

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
src/intl/        `Intl` (the `intl` feature: ICU4X);  data/intl/  its locale data
src/realm.rs     realms;  src/shadow.rs  ShadowRealm;  src/agent.rs  test262's agents
tests/           differential tests;  tests/octane/  feature scripts per benchmark
bench/micro/     one pattern per file;  bench/bytecode/  handwritten bytecode
bench/gc/        the collector's microbenchmarks;  bench/sparse/  sparse-array workloads, sweep
bench/compare/   against other engines (scripts/compare/)
docs/            designs (the design log is the commit messages)
```

