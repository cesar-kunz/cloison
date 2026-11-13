# Real web applications against other engines

Fifteen real, open-source applications, each bundled into one script that
runs headless (a small DOM and canvas of its own), boots, renders, and then
goes through a scripted session through its own APIs and input events. For
example: the TypeScript 6 compiler and language service (Apache-2.0) checking
a project, pdf.js (Apache-2.0) rendering documents, Excalidraw (MIT) panning
and restyling a large scene, Apache ECharts (Apache-2.0) drawing charts,
Sass (MIT) building Bootstrap, SheetJS CE (Apache-2.0) reading workbooks,
MathJax (Apache-2.0) typesetting formulas, JSNES (Apache-2.0) emulating a
console, CyberChef (Apache-2.0), PCjs (MIT) booting FreeDOS, TiddlyWiki
(BSD-3-Clause). Each app reports a hash of the work it did, checked against
a reference run.

Measured on the host and engine builds of [octane.md](octane.md) (cloison
of 2026-11-13, the other engines on 2026-11-11),
by `scripts/compare/webapps.py` (results in `bench/compare/webapps.tsv`):
one run per engine and app, the process's wall time from start to exit and
its peak RSS. cloison's runs are at a load of 2 to 4; the other engines' at
15 to 40 (other users' jobs on the shared host), so their times may be
somewhat pessimistic. cloison runs its `intl` build (TypeScript's
completions need `Intl.Collator`).

✗ marks a run that failed (an exception, a crash, a missing feature),
≠ one that finished but did other work than the reference; neither counts
in a geomean, and a geomean over fewer apps says how many.

## In short

- cloison runs 11 of the 15 apps correctly, with or without its JIT.
- **Interpreted, it is the fastest interpreter measured**: 1.14× V8's
  Ignition, 1.34× JavaScriptCore's LLInt, 1.7× QuickJS and ARK, 2.9×
  SpiderMonkey's interpreter (each over the apps both run), and first of
  all the interpreters on pdf.js, JSNES, ECharts, CyberChef, PCjs and
  SiYuan.
- **With its JIT it is twice as fast as ARK's JIT**, and about 5 to 6
  times slower than V8, JavaScriptCore and SpiderMonkey.
- The JIT gains less here than on Octane (1.3× over the interpreter,
  geomean): compiled code exits to the interpreter at an instruction that
  has no stencil, and generators and async functions run interpreted.

## Wall time with a JIT, seconds (lower is better)

| | cloison | V8 | JSC | SpiderMonkey | ARK JIT |
|---|---:|---:|---:|---:|---:|
| excalidraw | 61.6 | 9.77 | 9.40 | 9.81 | ✗ |
| pdfjs | 79.5 | 11.8 | 9.70 | 12.2 | ✗ |
| typescript | 82.7 | 12.5 | 16.9 | 19.1 | ✗ |
| jsnes | 49.8 | 9.00 | 9.07 | 15.0 | 125 |
| sass | 51.3 | 7.23 | 8.05 | 10.7 | 73.3 |
| sheetjs | 68.0 | 13.6 | 14.6 | 13.8 | 684 |
| echarts | 54.4 | 8.28 | 9.83 | 10.3 | 88.1 |
| mathjax | 18.8 | 3.23 | 3.65 | 3.77 | 24.4 |
| snap | ≠ | 10.2 | 8.34 | 11.0 | ✗ |
| cyberchef | 23.6 | 7.18 | 9.60 | 6.80 | ✗ |
| beepbox | ≠ | 2.24 | 2.91 | 2.33 | 29.2 |
| pcjs | 33.4 | 4.10 | 2.89 | 5.57 | 36.9 |
| univer | ✗ | 76.3 | 66.1 | 93.6 | ✗ |
| siyuan | 43.6 | 9.09 | 8.85 | 10.1 | 79.2 |
| tiddlywiki | ≠ | 8.22 | ≠ | 9.80 | ✗ |
| **geomean** | **47.0** (11) | **8.73** | **9.01** (14) | **10.4** | **76.3** (8) |

## Wall time without a JIT, seconds (lower is better)

| | cloison | V8 `--jitless` | JSC no JIT | SpiderMonkey no JIT | QuickJS | ARK |
|---|---:|---:|---:|---:|---:|---:|
| excalidraw | 66.2 | 59.0 | 64.9 | 150 | ✗ | ≠ |
| pdfjs | 115 | 141 | 142 | 320 | 197 | ✗ |
| typescript | 84.0 | 75.7 | 96.7 | 201 | ✗ | 91.8 |
| jsnes | 152 | 262 | 250 | 665 | 326 | 276 |
| sass | 62.0 | 58.0 | 74.2 | 175 | 140 | 69.6 |
| sheetjs | 70.4 | 54.4 | 94.0 | 132 | 105 | 533 |
| echarts | 100 | 206 | 185 | 435 | ✗ | 157 |
| mathjax | 14.5 | 14.6 | 13.8 | 47.7 | 15.1 | 11.5 |
| snap | ≠ | 43.9 | 66.4 | 146 | 89.3 | ✗ |
| cyberchef | 44.5 | 57.3 | 80.2 | 96.0 | 79.2 | ✗ |
| beepbox | ≠ | 38.7 | 43.7 | 91.1 | 54.6 | 61.9 |
| pcjs | 40.1 | 48.7 | 52.2 | 155 | 68.4 | 73.3 |
| univer | ✗ | 442 | 663 | 1,473 | ✗ | ✗ |
| siyuan | 41.9 | 43.4 | 66.8 | 110 | 84.5 | 61.6 |
| tiddlywiki | ≠ | 16.9 | ≠ | 44.3 | ✗ | ✗ |
| **geomean** | **61.3** (11) | **66.9** | **89.6** (14) | **175** | **89.2** (10) | **92.1** (9) |

KetteVM runs none of the apps yet (it doesn't parse regular expressions).

## Since the runs of 2026-11-11

Since the runs of 2026-11-11 (cloison of 2026-11-09), the interpreter's
geomean went from 101 s to 61 s and the JIT's from 97 s to 47 s: array
destructuring, `for…of` and spread skip the iterator protocol while
arrays' iteration is untouched (Excalidraw 164 s → 66 s), for-in is
linear on large objects (SheetJS 13 minutes → 70 s), string slices share
their text (CyberChef), and compiled code keeps typed-array and indexed
stores on fast paths after a buffer is detached or an array frozen
(ECharts with the JIT 110 s → 54 s).

## Open issues

- **Correctness**: Snap!, BeepBox and TiddlyWiki finish but do other work
  than the reference in some kernels, with and without the JIT; Univer
  needs `Intl.Segmenter` and `Intl.DateTimeFormat`, which the optional
  `Intl` doesn't have.
- Peak memory is about twice V8's and SpiderMonkey's here: see
  [memory.md](memory.md).
