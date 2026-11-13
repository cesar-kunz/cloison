# Memory and size against other engines

Measured on the host and engine builds of [octane.md](octane.md) (cloison
on 2026-11-13, Octane at b881c56, the rest at 00bd4d1; the other engines
on 2026-11-11).
Peak RSS is `/usr/bin/time`'s maximum resident set, one run each; the
empty script's figures are by `scripts/compare/startup.py` (`bench/compare/startup.tsv`).

## In short

- **One 6.4 MB static binary, no other files**, against 36 to 45 MB plus ICU
  data or libraries for V8, JavaScriptCore, SpiderMonkey and ARK.
- **An empty script runs in 2.0 ms and 4.5 MB**, next to QuickJS (1.5 ms,
  3.0 MB) and 5 to 12 times faster and 3.7 to 14 times smaller than V8,
  JavaScriptCore, SpiderMonkey and ARK.
- **Octane's peak RSS with the JIT is 76 MB** (geomean), about V8's (67) and
  SpiderMonkey's (82), a third of JavaScriptCore's (218); **without a JIT
  51 MB**, below every engine but QuickJS, which counts references.
- On the web apps the peak is about twice V8's and SpiderMonkey's (JIT
  geomean 765 MB against 379 and 381, JavaScriptCore 715): objects take 2
  to 4 times V8's bytes (V8 compresses pointers to 4 bytes), and the
  collector's blocks stay fragmented. Evacuation of fragmented blocks
  (`docs/gc.md`) took 5 to 24% off the peaks of the apps that fragment most.

## Size and an empty script

| | cloison | V8 | JSC | SpiderMonkey | QuickJS | KetteVM | ARK |
|---|---:|---:|---:|---:|---:|---:|---:|
| binary, stripped, MB | 6.4 | 35.6 | 0.5 + 36.3 library | 44.7 | 2.9 | 3.7 | 0.03 + 45.2 library |
| other files it needs | — | 10.8 MB ICU data | 39 MB ICU libraries | 3 NSPR libraries | — | — | ICU data |
| linked | static | dynamic | dynamic | dynamic | dynamic (libc) | dynamic (libc) | dynamic |
| empty script: wall ms | 2.00 | 13.1 | 15.0 | 11.4 | 1.45 | 1.92 | 25.1 |
| empty script: million instructions | 6.5 | 30.8 | 79.2 | 49.5 | 4.0 | 5.7 | 88.5 |
| empty script: RSS MB | 4.5 | 16.5 | 31.5 | 19.5 | 3.0 | 5.2 | 62.2 |

cloison's `intl` build (`Intl.Collator` on ICU4X) is 6.6 MB, its locale
data a file read at run time. V8, JavaScriptCore and SpiderMonkey carry all
of `Intl`; cloison, QuickJS and KetteVM don't, much of the difference.

## Octane's fixed work, peak RSS in MB, with a JIT (lower is better)

| | cloison | V8 | JSC | SpiderMonkey | ARK JIT |
|---|---:|---:|---:|---:|---:|
| richards | 15.8 | 30.8 | 78.5 | 25.5 | 101 |
| deltablue | 59.2 | 38.9 | 319 | 49.6 | 125 |
| crypto | 21.0 | 34.5 | 170 | 34.5 | — |
| raytrace | 48.0 | 35.2 | 333 | 32.5 | 112 |
| earley-boyer | 128 | 98.1 | 345 | 175 | — |
| regexp | 54.8 | 47.1 | 354 | 36.3 | 153 |
| splay | 508 | 247 | 728 | 361 | 603 |
| navier-stokes | 12.0 | 33.0 | 48.0 | 26.2 | — |
| pdfjs | 185 | 137 | 378 | 202 | — |
| mandreel | 132 | 80.7 | 236 | 110 | — |
| gbemu | 86.3 | 70.8 | 117 | 69.4 | — |
| code-load | 165 | 106 | 200 | 144 | — |
| box2d | 45.6 | 52.8 | 102 | 73.3 | 119 |
| zlib | 69.8 | 53.3 | 210 | 135 | — |
| typescript | 372 | 180 | 446 | 251 | — |
| **geomean** | **76.1** | **66.7** | **218** | **81.6** | **158** (6) |

## Octane's fixed work, peak RSS in MB, without a JIT

| | cloison | V8 `--jitless` | JSC no JIT | SpiderMonkey no JIT | QuickJS | ARK | KetteVM |
|---|---:|---:|---:|---:|---:|---:|---:|
| richards | 12.8 | 18.8 | 57.0 | 21.0 | 11.2 | 65.2 | — |
| deltablue | 31.5 | 23.2 | 65.2 | 64.9 | 11.2 | 94.5 | 12.0 |
| crypto | 15.8 | 21.8 | 65.6 | 29.5 | 11.2 | — | — |
| raytrace | 18.8 | 36.8 | 291 | 74.2 | 11.2 | 78.8 | — |
| earley-boyer | 92.0 | 85.5 | 294 | 170 | 17.0 | — | — |
| regexp | 29.2 | 30.8 | 128 | 37.3 | 11.2 | 119 | — |
| splay | 395 | 274 | 303 | 389 | 144 | 523 | 768 |
| navier-stokes | 12.0 | 24.8 | 29.2 | 22.5 | 11.2 | — | — |
| pdfjs | 130 | 122 | 298 | 177 | 38.9 | — | — |
| mandreel | 128 | 62.2 | 115 | 89.2 | 56.8 | — | — |
| gbemu | 77.3 | 81.8 | 62.5 | 80.5 | 23.2 | — | — |
| code-load | 164 | 101 | 185 | 143 | 74.2 | — | — |
| box2d | 31.4 | 103 | 61.1 | 93.0 | 12.0 | 81.0 | — |
| zlib | 13.9 | 24.0 | 33.0 | 25.3 | 134 | — | — |
| typescript | 262 | 168 | 173 | 207 | 137 | — | — |
| **geomean** | **50.6** | **55.9** | **108** | **75.5** | **27.6** | **116** (6) | **96.0** (2) |

## The web apps, peak RSS in MB, with a JIT ([webapps.md](webapps.md))

| | cloison | V8 | JSC | SpiderMonkey | ARK JIT |
|---|---:|---:|---:|---:|---:|
| excalidraw | 1,092 | 636 | 1,120 | 420 | ✗ |
| pdfjs | 1,119 | 989 | 1,210 | 797 | ✗ |
| typescript | 4,424 | 726 | 2,785 | 1,262 | ✗ |
| jsnes | 146 | 196 | 285 | 269 | 196 |
| sass | 646 | 314 | 695 | 241 | 354 |
| sheetjs | 1,970 | 612 | 1,332 | 631 | 1,581 |
| echarts | 1,257 | 492 | 631 | 323 | 523 |
| mathjax | 536 | 212 | 486 | 215 | 351 |
| snap | ≠ | 229 | 564 | 368 | ✗ |
| cyberchef | 888 | 514 | 944 | 398 | ✗ |
| beepbox | ≠ | 133 | 236 | 157 | 242 |
| pcjs | 159 | 98.4 | 153 | 142 | 172 |
| univer | ✗ | 772 | 2,100 | 907 | ✗ |
| siyuan | 548 | 348 | 736 | 308 | 306 |
| tiddlywiki | ≠ | 653 | ≠ | 477 | ✗ |
| **geomean** | **765** (11) | **379** | **715** (14) | **381** | **356** (8) |

## The web apps, peak RSS in MB, without a JIT

| | cloison | V8 `--jitless` | JSC no JIT | SpiderMonkey no JIT | QuickJS | ARK |
|---|---:|---:|---:|---:|---:|---:|
| excalidraw | 908 | 520 | 439 | 324 | ✗ | ≠ |
| pdfjs | 973 | 501 | 343 | 641 | 308 | ✗ |
| typescript | 2,541 | 794 | 426 | 648 | ✗ | 529 |
| jsnes | 134 | 168 | 184 | 216 | 35.1 | 122 |
| sass | 378 | 234 | 194 | 222 | 152 | 324 |
| sheetjs | 1,738 | 596 | 505 | 668 | 652 | 1,533 |
| echarts | 736 | 447 | 265 | 273 | ✗ | 394 |
| mathjax | 472 | 186 | 335 | 177 | 130 | 386 |
| snap | ≠ | 221 | 363 | 313 | 183 | ✗ |
| cyberchef | 887 | 455 | 462 | 398 | 216 | ✗ |
| beepbox | ≠ | 113 | 98.5 | 144 | 62.9 | 189 |
| pcjs | 124 | 78.7 | 113 | 127 | 28.1 | 134 |
| univer | ✗ | 687 | 674 | 756 | ✗ | ✗ |
| siyuan | 514 | 254 | 266 | 226 | 180 | 236 |
| tiddlywiki | ≠ | 485 | ≠ | 394 | ✗ | ✗ |
| **geomean** | **604** (11) | **316** | **293** (14) | **316** | **133** (10) | **313** (9) |

The JIT adds the most on the TypeScript app: 4.4 GB of peak RSS with it,
2.5 GB without.
