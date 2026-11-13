# Octane against other engines

Measured on one Linux x86-64 host (AMD EPYC 9454P, Zen 4, 96 cores,
shared): cloison of 2026-11-13, the other engines on 2026-11-11. Made by `scripts/compare/octane.py`
(results in `bench/compare/octane.tsv`, tables by `scripts/compare/report.py`).

The engines, each run directly (not through a wrapper shell):

| engine | version | with a JIT | without |
|---|---|---|---|
| cloison | 2026-11-13 | default | `CLOISON_NO_JIT=1` |
| V8 (`d8`) | 15.8.30 | default | `--jitless` |
| JavaScriptCore | 323696 (GTK port) | default | `--useJIT=false` |
| SpiderMonkey | 159.0b1 | default | `--no-blinterp --no-baseline --no-ion --no-native-regexp` |
| QuickJS (Bellard's) | 2026-06-04 (19d6bd8) | — | interpreter only |
| KetteVM | 8706d6f, `--features fast` | — | interpreter only |
| ARK (OpenHarmony, `ark_js_vm`) | ets_runtime 362b88477a, es2abc a7487a1105 | `--compiler-enable-jit=true` | its asm interpreter |

Each benchmark is Octane's own harness and file (the `-score.js` scripts
`scripts/octane.sh` makes), one run per engine; the "fixed work" tables run
the deterministic `-run.js` (the same work for every engine) under
`perf stat`. Instruction counts don't depend on the host's load; scores and
times do (the load was 2 to 20 during these runs). ARK compiles every
script as strict mode code (`es2abc`), so it runs Octane with the harness's
`alert` declared and only where a benchmark is strict-clean: six of the
fifteen. KetteVM runs two (DeltaBlue and Splay; it doesn't yet parse
regular expressions or `~`). A geomean over fewer benchmarks says how many.

## In short

- **Without a JIT, cloison's interpreter is the fastest measured**: 1.33×
  V8's Ignition (`--jitless`), 1.72× JavaScriptCore's LLInt, 2.5× QuickJS,
  3.7× SpiderMonkey's interpreter, 1.8× ARK's asm interpreter (on its six),
  first on 13 of the 15 benchmarks, and it does the fixed work in the
  fewest instructions (0.62× V8's, 0.46× QuickJS's).
- **With its JIT**, cloison scores 0.38× V8, 0.41× JavaScriptCore, 0.63×
  SpiderMonkey and 5.7× ARK's JIT (on its six). Against V8's tiers it is
  3.0× Sparkplug and 0.56× Maglev, ahead of Maglev on DeltaBlue and
  Richards ([octane-tiers.md](../octane-tiers.md)).
- Its peak RSS is close to V8's: see [memory.md](memory.md).

## Scores with a JIT (higher is better)

| | cloison | V8 | JSC | SpiderMonkey | ARK JIT |
|---|---:|---:|---:|---:|---:|
| richards | 42,581 | 51,509 | 60,349 | 24,207 | 6,065 |
| deltablue | 61,212 | 134,061 | 71,460 | 27,842 | 4,529 |
| crypto | 25,412 | 51,762 | 63,655 | 29,716 | — |
| raytrace | 42,771 | 161,392 | 165,684 | 49,949 | 3,384 |
| earley-boyer | 27,631 | 120,186 | 94,783 | 62,651 | — |
| regexp | 4,780 | 14,036 | 15,031 | 11,682 | 728 |
| splay | 18,452 | 82,404 | 58,708 | 46,935 | 10,043 |
| navier-stokes | 21,306 | 46,996 | 41,428 | 30,540 | — |
| pdfjs | 17,694 | 81,442 | 69,445 | 63,281 | — |
| mandreel | 35,001 | 75,508 | 70,359 | 56,836 | — |
| gbemu | 69,045 | 132,612 | 145,613 | 117,011 | — |
| code-load | 29,631 | 51,980 | 47,365 | 54,199 | — |
| box2d | 36,577 | 129,042 | 132,838 | 79,239 | 15,416 |
| zlib | 39,385 | 72,117 | 99,038 | 75,278 | — |
| typescript | 44,288 | 141,988 | 116,550 | 88,978 | — |
| **geomean** | **29,627** | **77,321** | **72,564** | **47,163** | **4,678** (6) |

## Scores without a JIT (higher is better)

| | cloison | V8 `--jitless` | JSC no JIT | SpiderMonkey no JIT | QuickJS | ARK | KetteVM |
|---|---:|---:|---:|---:|---:|---:|---:|
| richards | 2,927 | 2,060 | 1,931 | 471 | 1,140 | 1,837 | — |
| deltablue | 2,572 | 1,990 | 1,541 | 492 | 1,006 | 2,232 | 1,194 |
| crypto | 3,511 | 1,452 | 1,718 | 808 | 1,099 | — | — |
| raytrace | 6,988 | 5,579 | 2,738 | 1,461 | 2,243 | 4,534 | — |
| earley-boyer | 10,014 | 8,050 | 4,737 | 1,965 | 2,740 | — | — |
| regexp | 3,511 | 3,844 | 750 | 921 | 467 | 707 | — |
| splay | 13,843 | 11,358 | 12,117 | 3,940 | 6,532 | 9,358 | 2,462 |
| navier-stokes | 4,587 | 2,270 | 2,748 | 797 | 2,174 | — | — |
| pdfjs | 11,219 | 10,930 | 8,629 | 4,780 | 5,892 | — | — |
| mandreel | 5,221 | 3,338 | 2,794 | 977 | 2,946 | — | — |
| gbemu | 16,114 | 9,718 | 8,439 | 3,621 | 8,209 | — | — |
| code-load | 29,915 | 50,877 | 44,123 | 49,720 | 19,958 | — | — |
| box2d | 9,701 | 5,847 | 6,287 | 2,316 | 4,578 | 5,929 | — |
| zlib | 3,859 | 2,402 | 2,281 | 1,342 | 1,462 | — | — |
| typescript | 29,945 | 26,575 | 18,288 | 8,192 | 14,866 | — | — |
| **geomean** | **7,488** | **5,625** | **4,359** | **2,002** | **3,003** | **3,000** (6) | **1,715** (2) |

V8, JavaScriptCore and SpiderMonkey are ahead on CodeLoad, which measures
loading code: cloison compiles an `eval`'s functions to bytecode at their
first calls, but skims every one's tokens up front. V8 is ahead on RegExp.

## Fixed work without a JIT: G instructions (lower is better)

| | cloison | V8 `--jitless` | JSC no JIT | SpiderMonkey no JIT | QuickJS | ARK | KetteVM |
|---|---:|---:|---:|---:|---:|---:|---:|
| richards | 264 | 523 | 422 | 1,902 | 582 | 476 | — |
| deltablue | 309 | 495 | 570 | 1,821 | 612 | 441 | 771 |
| crypto | 313 | 1,157 | 614 | 1,690 | 1,133 | — | — |
| raytrace | 164 | 253 | 300 | 869 | 499 | 258 | — |
| earley-boyer | 276 | 382 | 424 | 1,443 | 824 | — | — |
| regexp | 39.2 | 38.4 | 175 | 142 | 278 | 168 | — |
| splay | 47.0 | 52.4 | 40.9 | 87.6 | 51.8 | 66.9 | 182 |
| navier-stokes | 169 | 446 | 259 | 613 | 280 | — | — |
| pdfjs | 32.6 | 36.5 | 32.0 | 66.5 | 53.3 | — | — |
| mandreel | 177 | 325 | 287 | 827 | 309 | — | — |
| gbemu | 45.2 | 87.5 | 76.1 | 204 | 78.6 | — | — |
| code-load | 9.20 | 6.36 | 5.57 | 6.32 | 18.1 | — | — |
| box2d | 42.2 | 79.6 | 56.4 | 202 | 77.8 | 63.8 | — |
| zlib | 433 | 1000 | 819 | 1,541 | 871 | — | — |
| typescript | 41.4 | 61.0 | 69.6 | 191 | 78.0 | — | — |
| **geomean** | **97.2** | **157** | **150** | **361** | **213** | **184** (6) | **375** (2) |

## Fixed work with a JIT: G instructions (lower is better)

| | cloison | V8 | JSC | SpiderMonkey | ARK JIT |
|---|---:|---:|---:|---:|---:|
| richards | 26.5 | 22.8 | 19.1 | 42.6 | 170 |
| deltablue | 16.8 | 8.74 | 12.4 | 36.6 | 200 |
| crypto | 94.2 | 39.6 | 34.6 | 66.5 | — |
| raytrace | 25.7 | 8.13 | 7.92 | 22.8 | 325 |
| earley-boyer | 105 | 31.1 | 25.1 | 49.0 | — |
| regexp | 31.0 | 11.1 | 10.2 | 10.9 | 165 |
| splay | 32.0 | 8.50 | 6.46 | 7.61 | 58.0 |
| navier-stokes | 45.0 | 14.3 | 10.9 | 19.1 | — |
| pdfjs | 18.5 | 7.10 | 6.67 | 5.78 | — |
| mandreel | 24.8 | 11.0 | 11.2 | 11.7 | — |
| gbemu | 12.8 | 8.45 | 7.11 | 6.26 | — |
| code-load | 9.24 | 6.40 | 5.57 | 5.24 | — |
| box2d | 11.6 | 5.15 | 6.60 | 6.20 | 29.0 |
| zlib | 66.0 | 34.2 | 22.3 | 29.9 | — |
| typescript | 22.0 | 13.0 | 15.7 | 10.2 | — |
| **geomean** | **27.8** | **12.4** | **11.5** | **15.6** | **120** (6) |

With the JIT, cloison runs about twice the instructions of the optimizing
engines for the same work: its stencils are per bytecode instruction, and
what isn't covered by stencils (newer instructions, most runtime calls)
runs in the interpreter or the runtime.
