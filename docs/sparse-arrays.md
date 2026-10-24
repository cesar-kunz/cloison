# Sparse arrays

Design for arrays whose elements are far apart. Today an array's elements
are always dense: `x = []; x[2 ** 27] = 1` allocates 2^27 holes (1 GB), and
an index or length from `MAX_ELEMENTS` (2^28) up throws
`RangeError: array index too large for this engine`. That has two costs:

- **Amplification.** One store can allocate up to 2 GiB. The fuzzer's
  `oom` on test262's `15.4_A1.1_T10` came from this (cloison-fuzz
  `triage/known-divergences.md`, "Memory of a far array write").
- **Conformance.** Lengths and indices from 2^28 to 2^32 − 2 are valid
  everywhere else ("Large array indices" in the same file). Of the 142
  test262 files in `built-ins/{Array,Object,Reflect,JSON,Function}` and
  `language` that use a length or index from 2^28 to 2^32, 18 fail on
  "too large for this engine" (cloison e2d511b). They are in
  `Array/length`, `Array/S15.4.5.2_*`, `indexOf`, `lastIndexOf`, `push`,
  `Object.defineProperty` and `Object.defineProperties`. Two more fail for
  unrelated reasons. The rest pass, each within 500 MB.

Goals, in order:

1. Memory proportional to the elements present, for any index up to
   2^32 − 2 and any length up to 2^32 − 1.
2. Dense arrays run exactly as now. The only new cost on their paths is a
   flag test in the `length` read (see "Compiled code").
3. Memory-safe by construction. A fast path that misses the new case falls
   to the runtime, or reads elements that really are there. It never reads
   past an allocation.

## What there is today

- `elems[0..elems_len)` holds the elements. A hole is `EMPTY`. For an
  array, `elems_len` *is* `length`: the interpreter, the stencils and the
  runtime all read it as such.
- `object::SPARSE` (flag 4) means "some integer keys are named properties
  of the shape" (their decimal strings). It is used for two things:
  - objects that aren't arrays, past a gap (`SPARSE_GAP`: an index past
    `2 * elems_len + 1024`);
  - elements with attributes or accessors, in arrays too
    (`define_own_property`: the element slot becomes a hole).

  Keyed accesses check those names after the elements.
- `props::slow_elements` gives an object a unique shape with
  `elems_code = NO_ELEMENTS` and no push cache. Every compiled store, append
  and push then fails `fits_code` / `push_transition` and goes to the
  runtime. Frozen and sealed arrays use this.
- `pop_class_flags` makes compiled `pop` exit on `SPARSE`, `SEALED`,
  `FROZEN` or `LENGTH_READ_ONLY` with one u16 load.

## Representation

A new array flag, `FAR`, means the following. It is bit 128, which is
already given a different meaning per class: `FN_NAME_DELETED` on
functions, `ERROR` on plain objects, `RESIZABLE` on buffers. On arrays,
strings and typed-array views the bit is free. That lets the `length`
stencils test it under one mask (see "Compiled code").

> This array's `length` is not `elems_len`. `elems[0..elems_len)` is still
> its dense prefix: real elements, holes as `EMPTY`. The length, and the
> elements past the prefix, live in the *far store*.

The far store is a hidden array, which JS never sees, whose value is in the
object's `code` bits. An array doesn't use `code` today. This is the same
arrangement as a `Map`'s entries (`collections`), so the collector traces
it the same way. Its layout:

```text
 0  length      number (up to 2^32 - 1)
 1  count       int32, live entries
 2  mutations   int32, bumped on each insertion (see "Builtins")
 3  mask        int32, buckets - 1 (a power of 2, minus one)
 4… key, value  open addressing, linear probing
                key: int32 or number (an index up to 2^32 - 2)
                EMPTY: a free bucket; a deleted one: key -1
```

It holds plain data elements only (writable, enumerable, configurable).
An element with attributes or an accessor stays where it is today: a named
property of the shape, under `SPARSE`. `FAR` and `SPARSE` are independent.
`FAR` concerns where the length and the plain far elements are. `SPARSE`
concerns the elements that have attributes. Both may be set on one array.

Phase 1 is arrays only. A plain object's far keys stay shape names. Moving
them to the far store would end the shape growth that
`o[k * 5000] = …` causes (one transition per key), but nothing needs it yet.

### Invariants

- `FAR` ⇒ `class == Array`, `slow_elements` applied, `code` is the far store.
- `FAR` ⇒ `elems_len ≤ length`. No far key is below `elems_len`, and none
  is at or past `length`.
- Not `FAR` and `class == Array` ⇒ `length == elems_len ≤ MAX_ELEMENTS`, as
  now.

## Transitions: one density policy

Dense storage is faster. Far storage is smaller once most indices are
holes. A dense slot costs 8 bytes whether or not it holds an element. A far
entry costs about 16 bytes divided by the table's load, so 24 to 32 bytes.
On memory alone, the break-even density (present elements over length) is
therefore about 1/4. Speed pulls the threshold lower: hole-heavy dense
arrays are still worth keeping up to some waste. So the choice is one
policy with three parameters, not the separate rules (2x plus 1024, a
preallocation limit, a density check) of a first draft:

| Parameter | Meaning | Starting point |
|---|---|---|
| `dense_min` (D) | an array stays or becomes dense only while present / length ≥ D | 1/8 |
| `dense_back` (D') | a far array goes back to dense once present / length ≥ D' (D' > D: hysteresis, so an array near the line doesn't flip back and forth) | 1/2 |
| `waste_floor` (W) | an array whose holes take at most W bytes stays dense whatever its density: small arrays and moderate preallocation never go far | 1 MiB (2^17 holes) |

Dense arrays don't keep a count of present elements, so the policy uses
the bound it has at each decision point:

- **A store at `i ≥ elems_len`** (`a[i] = v`, `defineProperty`). The new
  length is `i + 1`, and at most `elems_len + 1` elements are present. Go
  far if `(i + 1 − elems_len) * 8 > W` and `(elems_len + 1) / (i + 1) < D`.
  An append (`i == elems_len`) never qualifies. With D = 1/2 and W = 8 KiB,
  this is close to today's object rule (`2 * elems_len + SPARSE_GAP`).
  It takes the larger of the two terms where today's rule adds them, so
  objects that aren't arrays can share it. The JIT's store helper repeats the rule
  at `jit.rs:588`; make both call one function.
- **A length that grows with no elements** (`a.length = n`, `new Array(n)`,
  ArraySetLength, an array literal's trailing holes): the same test with
  `n` as the new length. `new Array(n)` followed by a fill stays dense, and
  compiled, up to `n = W / 8`, which is 131,072 with the starting values. That covers every size Octane uses (navier-stokes: 16,900).
- **`reserve_elems` growing the elements.** It copies the old elements
  anyway, so it counts the present ones during the copy, at no extra
  cost. If the count is below `D * n` and the holes exceed W, the array
  goes far instead of growing. This closes the chain that the store test
  alone allows (`a[1024]`, `a[3072]`, `a[7168]`, … stay just under the
  bound each time and would reach 2 GB in 18 stores).
- **Index ≥ `MAX_ELEMENTS`**: always far, whatever the density. That store
  no longer throws.

Going far takes these steps:

1. Allocate the store with `length` set to the new length.
2. Keep the dense prefix as it is.
3. Apply `slow_elements`.
4. Set `FAR`.

### Far → dense

- **Appends.** A store at `i == elems_len` extends the prefix. Then it pulls
  in keys `elems_len, elems_len + 1, …` from the store for as long as they
  are present. So `a[1e6] = 1` followed by a fill from 0 keeps a dense
  prefix that grows, with one far key.
- **Back to dense.** When `elems_len + count ≥ D' * length` and
  `length ≤ MAX_ELEMENTS`, move every key into the dense elements, drop
  `FAR` and the store, and give the array a shared shape again. That is
  `array_shape` if it has no named properties, with `NO_ELEMENTS`, so the
  next push picks the elements' type again. This is checked where the
  count grows past a power of two (the table's rehash, amortized) and where
  the prefix catches up with the length.

### Settings

The policy runs only in the runtime: in `set_keyed`'s slow paths,
`set_array_length`, `define_own_property` and `reserve_elems`. Compiled
code only exits to it. So the parameters can be read at run time with no
cost to any fast path. `Vm::new` reads them once into `Vm` fields, as
`CLOISON_GC_NURSERY` and the other heap settings are read:

- **Run time.** `CLOISON_ARRAY_DENSE_MIN`, `CLOISON_ARRAY_DENSE_BACK` (a
  fraction such as `1/8` or `0.125`) and `CLOISON_ARRAY_WASTE` (bytes, with
  `k`/`m` suffixes). A matching `--array-density D[,D'[,W]]` option in
  `bin/cloison.rs` is for one-off runs.
- **Build time.** The defaults are consts in `vm.rs`. Each can be
  overridden when building through `option_env!` with the same variable
  names, so an embedder builds its own defaults
  (`CLOISON_ARRAY_WASTE=64k cargo build --release`). A build-time default
  still yields to the run-time variable.
- `D = 1` with `W = ∞` means always dense: today's behaviour, except that
  `MAX_ELEMENTS` goes far instead of throwing. `D = 1` with `W = 0` means
  far as soon as there is any gap. These two ends bracket the sweep and
  make a regression easy to bisect to the policy.

### Choosing the defaults

The starting values above are guesses. The defaults come from a sweep.

The workloads go in `bench/sparse/`. Each file prints its time and does its
own checks:

| File | Pattern |
|---|---|
| `fill-forward.js` | `a = []`, then `a[i] = i` for i from 0 (pure appends) |
| `prealloc.js` | `new Array(n)`, then a fill from 0, for n from 10^3 to 10^7 |
| `fill-reverse.js` | `a[n − 1]` first, down to 0: far at first, it must come back to dense |
| `fill-random.js` | a random permutation of `[0, n)`: density rises from 0 to 1 |
| `strided.js` | every k-th index, k ∈ {2, 4, 8, 16, 64, 1024}: the region around D |
| `prefix-plus-far.js` | a dense prefix and a few far keys (`a[1e9] = x`) |
| `id-map.js` | integer ids in a large range (10^6 to 10^9) used as keys: stays far |
| `truncate.js` | a large length set, then cut back |
| `methods.js` | `forEach`, `indexOf`, `sort`, `join`, `slice` and `concat` on each of the shapes above |

For each workload and each point of the grid, record time (median of 5)
and peak RSS (`/usr/bin/time -f %M`, or the heap record). The grid is
D ∈ {1/2, 1/4, 1/8, 1/16, 1/32}, D' ∈ {2D, 4D}, W ∈ {8 KiB, 64 KiB,
1 MiB, 8 MiB}. Run the same files on V8, SpiderMonkey and JSC, for
reference on both axes.

Choose from the Pareto front, under these constraints:

- Octane runs and peak memory are unchanged against the always-dense
  setting. Only the `length` stencils' new test may move them.
- No workload is more than 2x slower than always-dense.
- No workload uses more than 1/D times the memory of its present elements,
  plus W.

Keep the sweep script (`bench/sparse/sweep.sh`) and its table in
`bench/`, as with `octane-memory.tsv`, so that the choice can be rerun
after the JIT changes.

## Runtime (Rust)

Each site below either reads `elems_len` as `length` or appends. Under
`FAR` each must use `far_length(o)`, or insert through `far_set`:

- `length` reads: `props::own_property` (`Value::from_i32(elems_len)`; it
  must become `Value::number` because a length can exceed i32), and
  `vm.rs` `get_named` and friends (the `matches!(class, Array | String |
  TypedArray)` sites near lines 2261, 2308, 2336, 2542, 2625 and 2723).
- `set_keyed`: a far store; `set_keyed_flagged` (`FAR` is a flag, so it
  already gets there); `set_array_length`; `define_array_length`;
  `grow_elements`; `set_element`.
- `get_keyed` and `sparse_get`, `has_own_index`, `own_property`
  (`Key::Index`), `delete_own`: look in the store after the prefix and
  before the shape names.
- `own_keys`: add the store's keys to `indices` before the sort that is
  already there.
- `define_array_length` truncation: today it loops over
  `(n..old_len).rev()` and interns every index. With `old_len = 2^32 − 1`
  that is 4 billion interns. Under `SPARSE`, loop over the shape's index
  names instead, and under `FAR`, over the store's keys, highest first,
  stopping at a non-configurable one.
- Fast paths that are already safe:
  - `array::dense()` requires `flags & !(EXTERNAL | PROTOTYPE) == 0`, so it
    excludes `FAR`;
  - `array_iterator_next` uses `length_of`;
  - `interp::to_string` of an array (`interp.rs:3374`) reads `elems_len`
    and must check `FAR`.
- GC: trace `code` for an `Array` with `FAR`. The heap record
  (`gc.rs:2076`, `elems_unused`) counts the store.

## Compiled code and the interpreter

The rule: a fast path may read and write `elems[0..elems_len)` of a `FAR`
array, because those are its real elements. It may never take `elems_len`
for its length, and never append.

| Path | Today | Under `FAR` |
|---|---|---|
| Loads `i < elems_len` (stencils, `interp` `GetKeyed`) | read `elems[i]` | unchanged: a real element, or a hole that goes to the runtime |
| Stores, appends and pushes (`set_keyed_ap_*`, `array_push_*`, the interpreter's append) | gated by `elems_code` | `slow_elements` gates them: runtime |
| `array_pop_` | `pop_class_flags` | add `FAR` to `POP_EXIT_FLAGS` |
| `get_length_` and `get_length_nt_` | class check only | read class and flags as one u16, as `pop_class_flags` does; `FAR` fails the match. No extra load |
| `get_length_nc_` (class known from a shape) | no check | test the flags byte, exit on `FAR` |
| Interpreter length IC (`poly_get_named`, `LENGTH_LOC`) | class check | add `flags & FAR == 0` |

Why `get_length_nc_` needs the test: `Ty::Ext(s)` facts survive calls. A
transition only appends slots, so a stale fact still reads correctly (see
the comment on `jit::Ty`). A callee can therefore make an array `FAR` while
the caller still knows it is an `Array`. Without the test, the caller would
read the prefix length, and type it `I32` even when the length is past
2^31. With the exit, compiled code never yields a `FAR` array's length, so
the `I32` type holds.

If the test shows up in an A/B, the alternative is invalidation. A
process-wide "an array has gone far" bit would let compiled code skip the
test until the first `FAR` array exists, and flipping the bit would then
discard the compiled code that relied on it. That needs a way to discard
code, so it is a fallback, not the plan.

## Builtins

Most of `Array.prototype` loops `k` from 0 to `len` with HasProperty. On a
`FAR` array with `length = 2^32 − 1`, that is 4 billion steps even when
there is one element.

When nothing can observe skipped indices, those loops jump to the next
present index instead. That holds when:

- the receiver is a `FAR` array;
- its prototype chain is `Array.prototype` then `Object.prototype`, with no
  elements or index names (the prototype clause of `dense()`);
- there is no Proxy.

The helper is `next_present(o, k, len) -> Option<u32>`. It returns the
smallest present index at or above `k`: the first present element of the
prefix, then the far keys taken from a sorted snapshot. If the store's
`mutations` count has changed since the snapshot was taken (a callback
added elements), the snapshot is taken again from `k`. The prototype
clause is re-checked at each step, which is constant time. Shape-named
index keys (`SPARSE`) join in through `own_keys`'s index list.

Which methods use it:

- **Skip holes, so jumping is exact:** `forEach`, `map` (its result is
  created with `length = len`, so it goes far too), `filter`, `some`,
  `every`, `reduce`, `reduceRight`, `indexOf`, `lastIndexOf` (downward),
  `flat`, `flatMap`, `concat`, `slice`, `splice`, `copyWithin`, `reverse`
  (pairs from the two ends), and `sort` (SortIndexedProperties collects the
  present elements, sorts them, writes them back from 0 and deletes the
  rest).
- **`includes`:** holes count as `undefined`. Search present elements, and
  if the value sought is `undefined` and any index is missing, the answer
  is true.
- **`join` and `toString`:** the output has `len − 1` separators. Check
  that against the longest string before looping, and throw
  `RangeError: Invalid string length` up front. JSON.stringify (`null` per
  hole) does the same.
- **Callbacks on every index and results that are dense by spec** (`find`,
  `findIndex`, `findLast`, `fill`, `keys`/`values`/`entries`, `toReversed`,
  `toSorted`, `with`, `toSpliced`): O(length) as the spec says. The
  copying ones hit `MAX_ELEMENTS` and throw, as today.

## Plan

1. **Store and flag.** `FAR`, the far store, `far_length` and
   `far_get`/`far_set`/`far_delete`. All the runtime sites above. The
   density policy with its settings, without going back to dense. The fast-path guards: the length
   stencils, the interpreter's length IC and `POP_EXIT_FLAGS`. Remove the
   "too large for this engine" errors where an index or length is the cause.

   Checks:
   - test262 `built-ins/Array`, `built-ins/Object`, `language`;
   - an Octane A/B for the `get_length_*` test;
   - a memory record of `x = []; x[2 ** 27] = 1` and of `15.4_A1.1_T10`;
   - the fuzzer's far-write cases against sm/jsc/qjs.
2. **Builtins.** `next_present` and the method list above.
   `define_array_length` truncation over keys. The up-front `join` and JSON
   size checks.
3. **Density.** The check in `reserve_elems`, going back to dense, the
   `bench/sparse` workloads and the sweep. Then fix the defaults. Optionally,
   plain objects' far keys move to the far store under the same policy.

## Open questions

- A preallocation past the dense limit (`new Array(5e6)` then a fill from 0)
  appends through the runtime until the prefix reaches `D' * length`, and
  then goes back to dense. The `prealloc.js` row of the sweep measures
  that cost. If it is too high, treat a fill from the prefix's end as
  evidence of intent, and go dense early once the prefix passes W.
- Overwrites in the prefix of a `FAR` array go to the runtime, because
  `slow_elements` gates every store. That is acceptable for a rare case. If
  it matters, give `FAR` arrays a shape whose `elems_code` admits in-place
  stores but no push cache, so appends still exit.
- Arguments objects and arrays with a frozen or sealed prefix: `FAR`
  composes with `FROZEN` and `SEALED` through the existing flag paths.
  Check each in `set_keyed_flagged`.
