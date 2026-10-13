# Octane in toi's and V8's tiers

Made by `scripts/octane-tiers.sh` (also written to the ignored `perf-history/octane-tiers-<commit>.{md,tsv}`).

## 93e5b2d

Octane scores (higher is better; medians of 3 runs, 8 at a time on cores of their own): toi 93e5b2d, V8 14.6.202.34-node.28 capped at each tier. Load average at the start and end: 32.70 17.94.

| benchmark | toi interp | toi JIT | Ignition | Sparkplug | Maglev | Turbofan |
|---|---:|---:|---:|---:|---:|---:|
| richards | 2171 | 33830 | 2081 | 2785 | 40876 | 50803 |
| deltablue | 2470 | 58858 | 2107 | 2860 | 57708 | 119032 |
| crypto | 1670 | 19286 | 1501 | 2466 | 36293 | 50373 |
| raytrace | 6204 | 10941 | 5386 | 7341 | 84655 | 132754 |
| earley-boyer | 6454 | 11654 | 8471 | 13502 | 63621 | 96391 |
| regexp | 2400 | 3263 | 5206 | 6630 | 11649 | 11875 |
| splay | 5468 | 5957 | 6681 | 7257 | 34543 | 34198 |
| navier-stokes | 3588 | 7656 | 2236 | 3116 | 38173 | 47441 |
| pdfjs | 8239 | 8662 | 10678 | 16440 | 69376 | 64285 |
| mandreel | 4327 | 25342 | 3321 | 4832 | 55331 | 72432 |
| gbemu | 13288 | 40020 | 9551 | 14378 | 121610 | 100613 |
| code-load | 12000 | 11644 | 45317 | 44778 | 45331 | 45165 |
| box2d | 7843 | 16495 | 5611 | 8255 | 115060 | 113747 |
| zlib | 3032 | 22170 | 109623 | 109466 | 109623 | 109623 |
| typescript | 25780 | 22937 | 25184 | 34480 | 81473 | 76214 |
| **geomean** (15) | **5224** | **15360** | **7073** | **9502** | **55698** | **64853** |

toi interp vs Ignition 0.74x (ahead on 9), Sparkplug 0.55x (ahead on 1), Maglev 0.09x (ahead on 0), Turbofan 0.08x (ahead on 0) (of 15; above 1x: toi scores higher)
toi JIT vs Ignition 2.17x (ahead on 9), Sparkplug 1.62x (ahead on 8), Maglev 0.28x (ahead on 1), Turbofan 0.24x (ahead on 0) (of 15; above 1x: toi scores higher)
