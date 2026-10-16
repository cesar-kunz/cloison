# Octane in toi's and V8's tiers

Made by `scripts/octane-tiers.sh` (also written to the ignored `perf-history/octane-tiers-<commit>.{md,tsv}`).

## 213fcdf

Octane scores (higher is better; medians of 3 runs, 8 at a time on cores of their own): toi 213fcdf, V8 14.6.202.34-node.28 capped at each tier. Load average at the start and end: 32.70 17.94.

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

## 6ef4928: toi on the whole core too

From here on toi gets the whole core, as Node does: its marker thread had
competed with the program for one CPU (splay +40%). Same code as 213fcdf.

Octane scores (higher is better; medians of 3 runs, 8 at a time on cores of their own): toi 6ef4928, V8 14.6.202.34-node.28 capped at each tier. Load average at the start and end: 6.76 26.82.

| benchmark | toi interp | toi JIT | Ignition | Sparkplug | Maglev | Turbofan |
|---|---:|---:|---:|---:|---:|---:|
| richards | 2754 | 32266 | 1689 | 1238 | 40558 | 51011 |
| deltablue | 2728 | 58488 | 1374 | 2006 | 57133 | 116513 |
| crypto | 2033 | 19371 | 1167 | 1979 | 36227 | 50323 |
| raytrace | 6821 | 11100 | 5081 | 7311 | 83989 | 132458 |
| earley-boyer | 8020 | 11815 | 8330 | 13547 | 62992 | 95730 |
| regexp | 2796 | 3228 | 5201 | 6650 | 11523 | 11886 |
| splay | 7142 | 7714 | 6636 | 6857 | 31812 | 34608 |
| navier-stokes | 3288 | 7701 | 2182 | 3095 | 38173 | 47393 |
| pdfjs | 8261 | 8597 | 10648 | 16284 | 67232 | 63155 |
| mandreel | 4326 | 25594 | 2146 | 4830 | 55578 | 74056 |
| gbemu | 13333 | 40996 | 9128 | 14365 | 120445 | 101209 |
| code-load | 11840 | 11488 | 23801 | 44947 | 44854 | 44925 |
| box2d | 7843 | 16479 | 2880 | 8068 | 114632 | 113319 |
| zlib | 2922 | 22089 | 63646 | 109545 | 109466 | 109702 |
| typescript | 26053 | 23200 | 19217 | 34221 | 80649 | 74783 |
| **geomean** (15) | **5638** | **15621** | **5562** | **8608** | **54985** | **64713** |

toi interp vs Ignition 1.01x (ahead on 10), Sparkplug 0.65x (ahead on 5), Maglev 0.10x (ahead on 0), Turbofan 0.09x (ahead on 0) (of 15; above 1x: toi scores higher)
toi JIT vs Ignition 2.81x (ahead on 11), Sparkplug 1.81x (ahead on 9), Maglev 0.28x (ahead on 1), Turbofan 0.24x (ahead on 0) (of 15; above 1x: toi scores higher)

## Steps 0–5 of the plan

Measured on the `-run.js` files against 6ef4928 (`perf stat`, medians of
3 interleaved runs), cycles: pdfjs −23% (instructions −26%, page faults
−42%), earley-boyer −15%, zlib −6%, gbemu −4% (page faults −45%), box2d
−3%; DeltaBlue's instructions unchanged. The binary is 25 KB smaller;
peak RSS from −5% (splay) to +9% (gbemu), from keeping freed large cells'
blocks. The toi JIT's geomean went from 15621 to 16346 (+4.6%); V8's
low tiers vary more between the two runs than toi does (load).

Octane scores (higher is better; medians of 3 runs, 8 at a time on cores of their own): toi with steps 0–5 of the plan below, V8 14.6.202.34-node.28 capped at each tier. Load average at the start and end: 7.10 12.35.

| benchmark | toi interp | toi JIT | Ignition | Sparkplug | Maglev | Turbofan |
|---|---:|---:|---:|---:|---:|---:|
| richards | 2778 | 33724 | 2058 | 2743 | 40668 | 51308 |
| deltablue | 2860 | 58951 | 2059 | 2804 | 57291 | 120738 |
| crypto | 1977 | 19300 | 1521 | 2442 | 36493 | 50339 |
| raytrace | 6935 | 10982 | 5175 | 7348 | 84285 | 132902 |
| earley-boyer | 9030 | 14173 | 8428 | 13690 | 62194 | 96935 |
| regexp | 2744 | 3188 | 5226 | 6668 | 11614 | 11851 |
| splay | 7412 | 7480 | 6720 | 7283 | 32737 | 35231 |
| navier-stokes | 3307 | 7934 | 2188 | 3104 | 38397 | 47441 |
| pdfjs | 10719 | 11935 | 10688 | 16331 | 66163 | 60628 |
| mandreel | 4323 | 25605 | 3354 | 4860 | 55716 | 72396 |
| gbemu | 13551 | 42441 | 9559 | 14365 | 119729 | 107033 |
| code-load | 11605 | 11279 | 45444 | 45023 | 44883 | 45102 |
| box2d | 7843 | 16825 | 5627 | 8286 | 115487 | 112889 |
| zlib | 3003 | 23777 | 109545 | 109623 | 109623 | 109702 |
| typescript | 27175 | 24035 | 25199 | 34545 | 83941 | 74261 |
| **geomean** (15) | **5834** | **16346** | **7045** | **9494** | **55264** | **64968** |

toi interp vs Ignition 0.83x (ahead on 12), Sparkplug 0.61x (ahead on 4), Maglev 0.11x (ahead on 0), Turbofan 0.09x (ahead on 0) (of 15; above 1x: toi scores higher)
toi JIT vs Ignition 2.32x (ahead on 11), Sparkplug 1.72x (ahead on 10), Maglev 0.30x (ahead on 1), Turbofan 0.25x (ahead on 0) (of 15; above 1x: toi scores higher)

## 668042a: after steps 0–12

V8's columns are reused from the 6ef4928+ run (V8 doesn't change with
toi). The toi JIT's geomean went from 15621 (6ef4928) to 18119: +16%.
By benchmark: raytrace +101%, earley-boyer +68%, pdfjs +49%, splay
+41%, typescript +10%, zlib +7%; DeltaBlue level.

Octane scores (higher is better; medians of 3 runs, 8 at a time on cores of their own): toi 668042a, V8 14.6.202.34-node.28 capped at each tier (V8's columns from perf-history/octane-tiers-6ef4928+.tsv). Load average at the start and end: 2.36 5.05.

| benchmark | toi interp | toi JIT | Ignition | Sparkplug | Maglev | Turbofan |
|---|---:|---:|---:|---:|---:|---:|
| richards | 2852 | 33742 | 2058 | 2743 | 40668 | 51308 |
| deltablue | 2820 | 58693 | 2059 | 2804 | 57291 | 120738 |
| crypto | 2030 | 19253 | 1521 | 2442 | 36493 | 50339 |
| raytrace | 7002 | 22355 | 5175 | 7348 | 84285 | 132902 |
| earley-boyer | 9187 | 19837 | 8428 | 13690 | 62194 | 96935 |
| regexp | 2799 | 3176 | 5226 | 6668 | 11614 | 11851 |
| splay | 8390 | 10868 | 6720 | 7283 | 32737 | 35231 |
| navier-stokes | 3363 | 7865 | 2188 | 3104 | 38397 | 47441 |
| pdfjs | 10782 | 12791 | 10688 | 16331 | 66163 | 60628 |
| mandreel | 4385 | 25664 | 3354 | 4860 | 55716 | 72396 |
| gbemu | 13728 | 41811 | 9559 | 14365 | 119729 | 107033 |
| code-load | 11845 | 11482 | 45444 | 45023 | 44883 | 45102 |
| box2d | 8623 | 16994 | 5627 | 8286 | 115487 | 112889 |
| zlib | 2984 | 23707 | 109545 | 109623 | 109623 | 109702 |
| typescript | 27309 | 25552 | 25199 | 34545 | 83941 | 74261 |
| **geomean** (15) | **5981** | **18119** | **7045** | **9494** | **55264** | **64968** |

toi interp vs Ignition 0.85x (ahead on 12), Sparkplug 0.63x (ahead on 5), Maglev 0.11x (ahead on 0), Turbofan 0.09x (ahead on 0) (of 15; above 1x: toi scores higher)
toi JIT vs Ignition 2.57x (ahead on 12), Sparkplug 1.91x (ahead on 10), Maglev 0.33x (ahead on 1), Turbofan 0.28x (ahead on 0) (of 15; above 1x: toi scores higher)

## Where the JIT gains little: findings and plan (at 213fcdf)

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

Done: 0–5 (see "Steps 0–5 of the plan" above). Step 4 measured smaller
than in the investigation (typescript −1.6% instructions, box2d −1.1%).
The barrier checker (`TOI_GC_CHECK_BARRIER`) also had a false positive
on splay: while a sweep is pending, a dead old cell it hadn't reached
held a stale pointer into memory already reused by young cells. The
checker now skips cells of the old color while sweeping.

Step 6, measured with `perf stat` instructions against 04becf3: box2d
−2.6%, pdfjs −4.2%, typescript unchanged. That is far below the estimate.
Functions that had been refused now run compiled, and that exposed three
other problems:

- **A bug.** A polymorphic call whose callee load had been dropped (its
  cases dispatch on the receiver's shape) left the callee's register
  unwritten when something between the load and the call exited.
  TypeScript's `table.add(symbol.name, symbol)` then called a stale value.
  Fixed separately; it costs nothing measurable.
- **Exits are expensive.** Each exit looked its instruction up by scanning
  the whole snapshot, thousands of instructions in TypeScript's inlining
  functions. An index makes typescript −1.8% on its own.
- **Spent functions keep failing.** A function out of compilations either
  keeps exiting at accesses no lowering will give up on, or finds no
  version its arguments fit (TypeScript's walker callbacks, called with
  every kind of node). Once one call in four fails, its calls are left to
  the interpreter.

The rest of box2d's and typescript's interpreter time is in calls from
compiled code into functions that run in the interpreter
(`preCollectTypes`: 1M calls through `toi_rt_call`), and in functions
refused or out of compilations for reasons other than named accesses.
Those need the call-path work in step 9 and a look at the compilation
budget (`max_compiles`).

Step 7: an array literal is `CreateArray n`, which allocates room for
its `n` elements (in compiled code `create_array_n`, the elements' cell
from the same bump allocation), followed by a `PushLiteral` per element.
The JIT lowers each `PushLiteral` to the `array_push` stencils, which
join the elements' type through their push cache. Measured with
`perf stat` against 571717c:

- splay −35.5% cycles, −37.5% instructions, peak RSS 778 → 650 MB;
- pdfjs −2.5% instructions;
- every other benchmark unchanged.

That's more than twice the estimate: the dead element arrays a literal
left behind had also cost the collector. A boilerplate to clone for
constant literals isn't needed for now: in compiled code, appending a
constant costs two stencils.

Step 8: `instanceof` goes through a two-entry cache per site. Each entry
holds a receiver's shape, a prototype and the result. A shape fixes its
prototype chain, so an entry stays valid for good. It needs no GC root
either: a `true` entry's prototype is on a chain that shapes keep alive,
and a `false` entry stays false even if its prototype dies and the
address is reused.

The stencil reads the function's `prototype` inline when the function has
the plain function shape. On a miss, the runtime answers and fills the
cache. Measured with `perf stat` against 19f666f: earley-boyer −9.2%
cycles (−12.7% instructions; a one-entry cache gave −6.8%), every other
benchmark unchanged.

Self calls (step 9's earley-boyer part). A self call binds directly to
the version being compiled only if its arguments fit that version's
entry types. `one_way_unify1_nboyer`'s entry is typed by its callers
(pairs), and it recurses on `lst1.car`, which is `Any`. So its 43M self
calls went through `toi_rt_call` → `vm_run` → `fitting_version`.

They aren't tail calls: the call is in a loop and its result is tested.
Now such a call gets a site of its own, claimed pending as for mutual
recursion. Its first call binds a version for its types, whose own
self calls fit it. Measured with `perf stat` against bfa73bc:
earley-boyer −18% cycles (−18.8% instructions), the others unchanged.

Left in earley-boyer:

- closure creation (`closure_of`, 8%);
- allocation (memset, `alloc_slow`, `free_young`, about 12%);
- `sc_list`, which reads `arguments`: entered through `vm_run` 13M times.

Calls from compiled code into functions running in the interpreter (at
232d4ae). Measured, not guessed:

- **A call through the runtime into compiled code is cheap,** about 250
  cycles. Earley-Boyer's 2.3M runtime calls to closures of an already
  bound function went direct: −0.7% instructions, −0.3% cycles.
  - These functions have no upvalues but are made anew by each call of
    the function making them (`conf_set_adjoin`). A site was bound to
    one closure.
  - Now a code made into more than one closure (`Code::closures_made`)
    is bound by code, as functions with upvalues were.
- **Polymorphic call sites wouldn't pay.** TypeScript's walker calls a
  callback that differs by walk (`pre`/`post`): about 700K calls to
  other functions than the bound one, under 1% of its time.
- **What costs is that the callees run interpreted.** `preCollectTypes`
  (1M calls), `preAssignScopes`, `emitParensAndCommentsInPlace` and
  others are refused for good, because they spend their compilations
  (32) on versions:
  - their entry is typed by the node they're called with, and the walker
    passes every kind;
  - each lowering again (8 for `preCollectTypes`) drops the versions;
  - so 19 compilations went to versions for new types and 6 to
    megamorphic generalizations, and almost none to exits (22 in all).

Tried, and dropped:

- *A last version taking any values once out of compilations, and
  widening to `Any` at a second generalization.* typescript +1.5%
  instructions, +3.5% cycles.
- *Twice the budget (64).* typescript +8.7% instructions, box2d +4.3%:
  more compiling, and the functions still don't settle.

The versions' policy, then. Versions already survive a lowering whose
code is the same; the churn was elsewhere:

- *An entry that went megamorphic forgot it at the next lowering,* and
  typed four more versions before joining again. Now each entry
  remembers its joined types, by bytecode offset, across lowerings, and
  its versions take them.
- *Then exits spent the budget.* `preCollectTypes` exited 48 times: each
  `ast.nodeType` test is a site of its own, and each needed four exits to
  be given up. Past half a function's compilations, a site whose cache
  is full and still misses is given up at its first exit.

Measured with `perf stat` against 4d55b75:

- typescript −5.2% cycles (−2.9% instructions);
- `preAssignScopes` and `emitParensAndCommentsInPlace` now run compiled,
  and so does `preCollectTypes`, partly through the runtime from callers
  compiled before it was;
- gbemu +1.3% instructions, +0.5% cycles, from remembering the joins.
  Applying them only past half the budget lost typescript's gain;
- the others unchanged, DeltaBlue's instructions included.

Step 12: Prototype's `Class.create` constructors.

Every class's constructor is one code,
`function () { this.initialize.apply(this, arguments); }`. Each `new`
went through the runtime and ran the constructor and its `initialize` in
the interpreter. Now `new C(…)` inlines it:

- *A forwarding function is inlined* when each of its forward calls
  calls a method of `this`.
  - Its frame takes a window past its registers.
  - Its forward call becomes an ordinary call of `g`, with `this` `x`
    and the `new`'s arguments, inlined or direct. That holds when `apply`
    and `g` come from prototype slots; else the call bails, and the
    interpreter finds the arguments in the caller's window.
  - Box2D's `b2Vec2.b2Vec2.apply(this, arguments)` takes `g` from a
    variable, so it isn't inlined: inlining it bailed, +6%.
- *`this.initialize` is loaded from the closure's prototype.* `this` has
  the root shape for that prototype, and the code's cache is every
  class's.
- *Not the code's slack tracking:* its objects' shapes are every class's.
- *Not the inlining budget:* past it, a `new` went through the runtime,
  in `renderScene` for one, with both functions interpreted.

Measured with `perf stat` against 96c93fc:

- raytrace −48.3% cycles (−56.9% instructions), 1.9×;
- the others unchanged;
- its JIT code 910 → 993 KB, peak RSS 48 → 51 MB, the binary +23 KB.
