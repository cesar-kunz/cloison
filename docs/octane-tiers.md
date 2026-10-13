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

## Where the JIT gains little: findings and plan (at 93e5b2d)

The JIT gains little over toi's interpreter on raytrace (1.8x), earley-boyer
(1.8x), box2d (2.1x), splay (1.1x), pdfjs (1.05x) and typescript (0.9x).
Here is each one's profile with the JIT on (share of samples):

| benchmark | interp | JIT code | JIT runtime | VM/builtins | GC+alloc | kernel |
|---|---:|---:|---:|---:|---:|---:|
| raytrace | 44 | 23 | 15 | 10 | 10 | 1 |
| box2d | 49 | 22 | 12 | 6 | 1 | 5 |
| typescript | 48 | 3 | 10 | 15 | 5 | 8 |
| earley-boyer | 8 | 20 | 27 | 28 | 13 | 1 |
| splay | 11 | 7 | 8 | 16 | 46 | 12 |
| pdfjs | 15 | 7 | 14 | 38 | 5 | 12 |

Measured gains below come from hacks in throwaway worktrees, measured with
`perf stat` on the `-run.js` files. Score A/B runs were too noisy on this
host: box2d's baseline ranged from 12.8K to 15.1K.

### Causes

**Code that stays in the interpreter** (raytrace, box2d, typescript)
- *Named accesses bail.* `select_property` has no generic path for named
  gets and sets: `Prop::None` gives the `bail` stencil.
  - After 4 property exits a named access is given up and becomes a bail;
    a keyed one becomes `Prop::Runtime`.
  - 3 bail exits at an entry put it in `no_entry`, so callers stay in
    the interpreter for good.
  - This explains 34% of box2d's and 38% of typescript's interpreter ops
    directly, and most of the rest indirectly: the small helpers that
    refused functions call can't be inlined.
  - It also catches pdfjs's inflate loop through `buffer.byteLength`, a
    native getter.
- *Small loop-free functions are never entered from the interpreter.*
  This covers 99.8% of raytrace's interpreter ops. Raytrace builds its
  classes with Prototype's `Class.create`, so every class shares one
  constructor, `function(){ this.initialize.apply(this, arguments) }`.
  - `CallApplyArguments` is a forward call that always goes through the
    runtime, and a function containing one is never inlined.
  - The constructor's `this.initialize` site is megamorphic, because all
    classes share that one Code.
  - Compiling small functions earlier made it worse (+18% cycles).
- *Cooling after a function has used up its compilations.* Calls still
  go to the interpreter while a function cools, though no new lowering
  will come. typescript's `innerScan` is entered 889K times in the
  interpreter and 1.6K times compiled.

**Runtime paths** (earley-boyer)
- *Open upvalues.* `Vm::open_upvalue` binary-searches a list averaging
  127 entries (deep recursion), loading a scattered cell on each probe.
  That's 105M calls: `closure_of` takes 12.7%.
- *Self-recursive calls through `toi_rt_call`.* 43M calls to
  `one_way_unify1_nboyer` with arguments read from `.car`. Its entry is
  typed by parameter shape, so `fits_entry` fails, and each call pays
  `vm_run` → `vm_prepare` → `fitting_version`'s scan: about 15–18%.
  The self-call site binds once for all versions.
- *`instanceof` always calls the runtime.* 245M runs of
  `test_instanceof_m`, each doing a generic `get` of `prototype`: about 9%.
- *Each closure gets a prototype object at creation*, doubling closure
  allocation: about 2–4%.

**Arrays and strings** (pdfjs, splay)
- *`slice` and `splice` copy the whole array.* `this_array` copies every
  element into a `Vec`, and `splice` writes the array back one element at
  a time through `array_push` → `store_element`. In pdfjs that's 110M
  `store_element` calls, its 12%, plus most of its malloc and free.
  `shift`, `unshift`, `concat` and `reverse` have the same problem.
- *Array literals grow one store at a time.* `[0,…,9]` is `CreateArray`
  plus 10 appending `SetKeyed`s. The first store goes to the runtime, and
  growing to 4, 8, then 16 takes the slow piece. That's about 17% of
  splay, and it leaves dead element arrays behind.
- *String builtins go through the generic runtime call.* `charCodeAt` and
  `fromCharCode` are about 6–7M calls each, and `fromCharCode` allocates
  a `Vec` per call. String equality flattens before comparing. Together:
  pdfjs about 15%.

**GC and memory** (splay, pdfjs)
- *Blocks are handed back to the OS as soon as they empty.* `release()`
  and `free_large` call `madvise(DONTNEED)` at once, so the next use pays
  a page fault, kernel zeroing and toi's own memset. That's the 12%
  kernel time in splay (393K faults) and in pdfjs.
- *Splay's data outlives the nursery.* A payload tree lives through about
  80 MB of allocation, so each minor collection marks about 220K
  survivors (7 ms). Major collections trace 122 MiB live, 21 times.
- *The tiers table puts toi at a disadvantage.* toi is pinned to one CPU
  while Node gets both SMT siblings, so toi's marker thread competes with
  the program. Splay scores 2755 on one CPU and about 3900 on the whole
  core. Splay's latency score is already level with V8's low tiers; its
  throughput is what lags.

### Plan

Rank each step by its effect on the benchmark, and check DeltaBlue for
regressions (`perf.sh --ab`).

| # | change | effort | expected gain |
|---|---|:-:|---|
| 0 | `octane-tiers.sh`: give toi the whole core, as Node gets; rerun the table | S | fairer table (splay +40%) |
| 1 | `open_upvalue`: scan from the top of the list | S | earley-boyer −14% cycles (measured) |
| 2 | `slice`/`splice`/`shift`/`unshift`/`concat`: copy only the range, move in place, one type join per batch | S | pdfjs +27% (measured) |
| 3 | Heap: keep emptied and large blocks committed; release the excess lazily; skip the memset on a released block | S | splay −9%, pdfjs +6% (measured) |
| 4 | Skip cooling once a function is out of compilations | S | typescript −3.8%, box2d −1.6% cycles (measured) |
| 5 | String equality: compare pointers and lengths before flattening; `fromCharCode` without a `Vec` | S | pdfjs, a few % |
| 6 | `get_named_rt`/`set_named_rt` stencils (megamorphic runtime lookup) instead of `bail` for an empty, megamorphic or given-up cache; refusals reversible | M | box2d, typescript +25–50% (estimate); pdfjs `byteLength` +6.5% |
| 7 | Array literals: `CreateArray n` with exact capacity and initializing stores; boilerplate clone for constant literals | S–M | splay ~15%; common elsewhere |
| 8 | Inline `instanceof` with a per-site (function, shape) cache | S–M | earley-boyer ~8% |
| 9 | Guarded self calls: check argument shapes inline against the version's entry types, fall back to the runtime; a site per version; cheaper `fitting_version` | M | earley-boyer ~15% |
| 10 | JIT intrinsics for `charCodeAt`/`fromCharCode` (like `push`/`pop`) | M | pdfjs ~5% |
| 11 | Lazy closure `prototype` | M | earley-boyer 2–4% |
| 12 | `new C(…)` with a known closure: inline the shared `Class.create` constructor, fold `this.initialize`, call `initialize` directly with the site's own arguments; slack tracking per closure | M–L | raytrace 2–3× |
| 13 | Pretenure allocation sites with high survival (card marks in the allocation stencils); meanwhile grow the nursery while survival is high | L (S) | splay ~15% (5–8%) |
| 14 | Huge pages for the heap, faster heap growth | S | splay −8 to −13%, at +25–45% RSS: a memory trade-off |

Order: 0–5 first (small and measured), then 6, the widest at medium
effort, then 7–9, then 12 and 13.
