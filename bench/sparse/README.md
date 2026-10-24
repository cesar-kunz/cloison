# Sparse-array workloads

The workloads behind the density settings of
[docs/sparse-arrays.md](../../docs/sparse-arrays.md) ("Choosing the
defaults"). Each file prints `name size ms checksum`; its sizes are
globals a prelude may set (`var N = 1e6; var K = 16;`), as `sweep.sh`
does.

| file | pattern |
|---|---|
| [forward.js](forward.js) | appends from 0, then a read pass: never sparse, the control |
| [prealloc.js](prealloc.js) | `new Array(N)`, then a fill from 0 and a read pass |
| [reverse.js](reverse.js) | a fill from N - 1 down to 0: far at first, dense on the way |
| [random.js](random.js) | a fill of `[0, N)` in a random order |
| [strided.js](strided.js) | every K-th index, N of them, then reads: density 1/K |
| [prefix-far.js](prefix-far.js) | a far key (`a[1e9]`), then a dense prefix of N filled, updated and read |
| [id-map.js](id-map.js) | N integer ids from `[0, 1e9)` as keys, then lookups |
| [many.js](many.js) | N arrays of ten elements and one key 20,000 past them |
| [methods.js](methods.js) | `forEach`, `indexOf`, `reduce`, `slice` and `sort` over every K-th index |

```sh
bench/sparse/sweep.sh [--reps 3] [--no-wait] [--points FILE] [--engines] target/release/cloison OUT.tsv
python3 bench/sparse/pareto.py OUT.tsv [--top N]
```

`sweep.sh` runs every workload size at every setting of the grid
(`--array-density D,D',W,P`; `--points` takes them one a line instead), and
with `--engines` on V8, SpiderMonkey and JSC (`~/.jsvu/bin`) too: the
median time of the runs, and the peak RSS. It resumes an OUT.tsv cut
short. Unless `--no-wait`, each run waits for a 1-minute load below 4.
`pareto.py` scores each setting by the geometric means of its time and
memory as multiples of the best setting per workload, its worst cases, and
the same against the engines' best, and lists the Pareto front.

[results.tsv](results.tsv) is the sweep the defaults (`1/16,1/8,8k,16m`)
come from: cloison 45d00fe, 3 runs a point, the host's load about 30 of
its 96 threads (single-threaded runs; read times to 10% or so). Its
summary:

| setting | time | memory | worst time | worst memory |
|---|---|---|---|---|
| `1/32,1/16,8k,16m` | 1.18 | 1.19 | 2.4, random N=1e6 | 2.1, random N=1e6 |
| **`1/16,1/8,8k,16m`** | 1.23 | 1.12 | 2.9, reverse N=1e6 | 2.1, strided K=16 |
| `1/8,1/4,8k,16m` | 1.41 | 1.11 | 5.2, reverse N=1e6 | 1.5, random N=1e6 |
| `1/8,1/2,8k,16m` | 1.65 | 1.21 | 9.7, reverse N=1e6 | 2.1, prealloc N=1e7 |
| `1/16,1/8,256k,16m` | 1.50 | 1.38 | 245, many | 48, many |

At the default, against the best of V8, SpiderMonkey and JSC per workload:
1.56 times the time and 1.06 times the memory (geometric means); faster
than all three on `prefix-far` (599 ms against 1,178), `reverse` and
`random`; about 3 times the best on `prealloc`, `methods` and `strided` K=2 and 4
(V8, with its fast holey arrays).
