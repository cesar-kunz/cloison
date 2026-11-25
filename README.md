# cloison

[![CI](https://github.com/cesar-kunz/cloison/actions/workflows/ci.yml/badge.svg?branch=master)](https://github.com/cesar-kunz/cloison/actions/workflows/ci.yml)
[![Nightly](https://github.com/cesar-kunz/cloison/actions/workflows/nightly.yml/badge.svg?branch=master)](https://github.com/cesar-kunz/cloison/actions/workflows/nightly.yml)
[![test262](https://img.shields.io/endpoint?url=https%3A%2F%2Fraw.githubusercontent.com%2Fcesar-kunz%2Fcloison%2Fbadges%2Ftest262.json)](docs/README-long.md#standards-support)
[![License: Apache-2.0](https://img.shields.io/badge/license-Apache--2.0-blue)](LICENSE)
[![Platforms](https://img.shields.io/badge/platforms-Linux%20x86--64%20%7C%20Linux%20arm64%20%7C%20macOS%20arm64-lightgrey)](docs/README-long.md)
[![Rust nightly-2026-10-06](https://img.shields.io/badge/rust-nightly--2026--10--06-orange)](rust-toolchain.toml)

A JavaScript engine in Rust: a register-bytecode interpreter and a
copy-and-patch JIT, in one small static binary.

<picture>
  <source media="(prefers-color-scheme: dark)" srcset="https://raw.githubusercontent.com/cesar-kunz/cloison/bench/chart-dark.svg">
  <img alt="Bar charts, measured nightly: Octane's geomean score with JITs off (each engine's interpreter) and with JITs up to a like tier (V8's Sparkplug and Maglev, JavaScriptCore's Baseline and DFG, SpiderMonkey's Baseline; the full engines fainter), an empty script's RSS and Octane's peak RSS; cloison against V8, JavaScriptCore, SpiderMonkey and QuickJS. The tables below have the figures." src="https://raw.githubusercontent.com/cesar-kunz/cloison/bench/chart-light.svg">
</picture>

- **Small**: one 6.5 MB static binary, no other files; an empty script runs
  in 2 ms and 4.5 MB of RSS.
- **The fastest interpreter measured on Octane**: 1.33× V8's Ignition, 1.7×
  JavaScriptCore's LLInt, 2.5× QuickJS, 3.7× SpiderMonkey's interpreter.
- **A copy-and-patch JIT** (x86-64 and arm64) that LLVM writes ahead of
  time: 3× V8's Sparkplug on Octane, ahead of Maglev on DeltaBlue and
  Richards, 5.7× OpenHarmony's ARK JIT (on the six benchmarks it runs).
- **Lean on Octane**: a 73 MB peak with the JIT (V8 67, SpiderMonkey 82,
  JavaScriptCore 218), 49 MB without.
- **Modern JavaScript**: ES5 through ES2026 and most Stage 3 proposals,
  98.4% of test262.
- **Real applications run**: TypeScript's compiler, pdf.js, Excalidraw,
  ECharts, Sass, MathJax and more; interpreted, the fastest interpreter
  measured: 1.09× V8's Ignition, 2.9× SpiderMonkey's interpreter.
- **A generational, concurrent collector** that evacuates fragmented blocks.
- **GC safety in the Rust code checked by the compiler**: an object held
  across a call that may collect without being rooted is a compile error
  ([how](#gc-safety-the-compiler-checks)).
- **Numbers match V8's bit for bit** (fdlibm `Math`, shortest round-trip
  printing), but for `pow`, within one ulp of it.
- **Linux** (x86-64, arm64) and **macOS** on Apple silicon; nightly Rust,
  one crate whose only runtime dependency is `libc`.

```sh
cargo build --release
target/release/cloison file.js            # prints the completion value
target/release/cloison --module main.mjs  # a module
target/release/cloison --dump file.js     # the bytecode
```

## Against other engines

Octane on Linux x86-64 (higher is better; geomeans over its 15 benchmarks):

| | cloison | V8 | JavaScriptCore | SpiderMonkey | QuickJS |
|---|---:|---:|---:|---:|---:|
| without a JIT | **7,493** | 5,625 | 4,359 | 2,002 | 3,003 |
| with a JIT | 29,719 | 77,321 | 72,564 | 47,163 | — |

Size and memory (lower is better):

| | cloison | V8 | JavaScriptCore | SpiderMonkey | QuickJS |
|---|---:|---:|---:|---:|---:|
| binary, MB | 6.5 | 35.6 + 10.8 ICU | 36.8 + 39 ICU | 44.7 | 2.9 |
| empty script, RSS MB | 4.5 | 16.5 | 31.5 | 19.5 | 3.0 |
| Octane peak RSS, JIT, MB | 73 | 67 | 218 | 82 | — |
| Octane peak RSS, no JIT, MB | 49 | 56 | 108 | 76 | 28 |

The reports, with every benchmark, the engines' versions and flags, ARK
and KetteVM:

- [Octane](docs/bench/octane.md): scores and instruction counts, with and without JITs
- [Real web apps](docs/bench/webapps.md): 15 open-source applications, time and what is still slow
- [Memory](docs/bench/memory.md): sizes, startup, peak RSS
- [V8's tiers](docs/octane-tiers.md): cloison against Ignition, Sparkplug, Maglev and Turbofan over time

## GC safety the compiler checks

The collector runs inside the engine's Rust code: a built-in such as
`Array.prototype.map` calls back into JavaScript, which may collect. An
object the Rust code holds in a local across such a call must be rooted,
or it may be freed while still in use. In cloison the type system tracks
this, in the manner of [gc-arena](https://github.com/kyren/gc-arena):

```rust
let raw = vm.get_b(o, "raw")?;        // a Fresh value: it still borrows the VM
let n = vm.length_of(o)?;             // error[E0499]: may collect while `raw` is held
```

```rust
let s = Scope::new(vm);
let raw = vm.get_b(o, "raw")?.root(&s); // rooted: safe across any call
let n = vm.length_of(o)?;
```

- what a call that may collect returns, and every new object or string,
  borrows the VM until it is rooted or used, so holding one across the
  next call doesn't compile;
- built-ins receive their arguments as a type only rooted values can
  become;
- the few places that take a raw value out for speed say why it is safe,
  and CI checks that each says so.

It costs nothing measurable on Octane, and turned up four real bugs on
the way. [docs/gc-branding.md](docs/gc-branding.md) explains it from the
start.

What the types leave open, a raw value held across a call, a static
analysis checks on every path, in CI: a rustc driver that follows each
local holding a cell to where it is rooted
([docs/gc-hazards.md](docs/gc-hazards.md)).

## More

- [cloison in depth](docs/README-long.md): how it works, design choices,
  standards support, testing, building, where it falls short
- [docs/gc.md](docs/gc.md): the collector
- [docs/gc-branding.md](docs/gc-branding.md): branded cells, the compiler checking GC safety in natives
- [docs/gc-hazards.md](docs/gc-hazards.md): the static analysis of unrooted cells, every path
- [docs/object-model.md](docs/object-model.md): objects, shapes, escape analysis
- [docs/direct-calls.md](docs/direct-calls.md): the call convention
- [docs/sparse-arrays.md](docs/sparse-arrays.md): sparse arrays and their defaults
- [docs/opcode-targets.md](docs/opcode-targets.md) and
  [docs/arm64-targets.md](docs/arm64-targets.md): stencils against handwritten code
- [docs/intl.md](docs/intl.md): the optional `Intl`
- [docs/sandbox.md](docs/sandbox.md): heap sandboxing, deferred
- The commit messages are the design log: each step with what it
  measured.

## License

Licensed under the Apache License, Version 2.0; see `LICENSE`.
