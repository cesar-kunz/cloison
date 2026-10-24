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
 2  used        int32, live and deleted entries
 3  mutations   int32, bumped by each new key (see "Builtins")
 4  mask        int32, buckets - 1 (a power of 2, minus one)
 5… key, value  open addressing, linear probing, at most 3/4 used
                key: an index as a number; EMPTY a free bucket, -1 a
                deleted one
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
entry costs 16 bytes divided by the table's load (at most 3/4), so 21 to
32 bytes. On memory alone, the break-even density (present elements over
length) is therefore about 1/3 to 1/4. Speed pulls the threshold lower.
Four parameters (`far::Density`) set the policy:

| Parameter | Meaning | Default |
|---|---|---|
| `min` (D) | an array stays or becomes dense only while present / length ≥ D | 1/16 |
| `back` (D') | a far array goes back to dense once present / length ≥ D' (D' ≥ D: hysteresis, so an array near the line doesn't flip back and forth) | 1/8 |
| `waste` (W) | an array whose holes take at most W bytes stays dense whatever its density | 8 KiB (1,024 holes) |
| `prealloc` (P) | W for a length set with no element (`new Array(n)`, `a.length = n`), mostly filled next from 0 | 16 MiB |

The defaults come from a sweep (see "Choosing the defaults").

Dense arrays don't keep a count of present elements, so the policy uses
the bound it has at each decision point (`far::goes_far`):

- **A store at `i ≥ elems_len`** (`a[i] = v`, a plain `defineProperty`).
  The new length is `i + 1`, and at most `elems_len + 1` elements are
  present: go far if the holes, `(i + 1 − present) * 8` bytes, pass W and
  `present / (i + 1) < D`. An append never qualifies.
- **A length that grows with no elements** (`a.length = n`, `new Array(n)`,
  ArraySetLength, an array literal's trailing holes): the same test with
  `n`, and P for W.
- **The elements reallocated.** Where the bound keeps the array dense but
  the store or length outgrows `elems_cap`, and the array is large enough
  for its holes to pass W, the present elements are counted: that costs
  what the copy does. Without it, `a[i * 16] = v` stays dense with 15 holes
  in 16 (each store is within the bound), and `a[1024]`, `a[3072]`,
  `a[7168]`, … reach 4 GB in 18 stores (now 7.6 MB).
- **Index ≥ `MAX_ELEMENTS`**: always far, whatever the density. That store
  no longer throws.

Going far (`far::make_far`): the store is allocated with the new length,
the elements up to the first hole stay the dense prefix, the ones after it
move to the store (and the elements shrink if that frees half of them), the
shape goes slow (`far_elements`: `slow_elements` without dropping compiled
code, since in-place stores into the prefix stay right), and `FAR` is set.

### Far → dense

- **Appends.** A store at `i == elems_len` extends the prefix. Then it pulls
  in keys `elems_len, elems_len + 1, …` from the store for as long as they
  are present. So `a[1e6] = 1` followed by a fill from 0 keeps a dense
  prefix that grows, with one far key.
- **Back to dense** (`far::maybe_dense`, after each far store and each
  truncation). Once `elems_len + count ≥ D' * length` (or the holes would
  fit in W) and `length ≤ MAX_ELEMENTS`, every key moves to the dense
  elements, `FAR` and the store go, and the array gets a shape whose
  elements take stores' fast paths again: `array_shape` moved to the
  elements' type if it has no properties, else a unique shape of that type.

### Settings

The policy runs only in the runtime: in `set_keyed`'s slow paths,
`set_array_length`, `define_own_property`, `grow_elements` and the `Array`
constructor. Compiled code only exits to it. So the parameters cost no fast
path anything, and are read once per VM (`Density::from_env`):

- **Run time.** `CLOISON_ARRAY_DENSE_MIN` and `CLOISON_ARRAY_DENSE_BACK`
  (fractions, `1/16` or `0.0625`), `CLOISON_ARRAY_WASTE` and
  `CLOISON_ARRAY_PREALLOC` (bytes, `k`/`m`/`g` suffixes, `inf`), or
  `--array-density D[,D'[,W[,P]]]` for a run.
- **Build time.** The same variables, set when building, become that
  build's defaults (`option_env!`), and still yield to the run-time ones:
  `CLOISON_ARRAY_WASTE=64k cargo build --release`.
- `0,0,inf,inf` is always dense (the behaviour before, except that
  `MAX_ELEMENTS` goes far instead of throwing); `1,1,0,0` is far at any gap.
  The two ends bracket the sweep.

### Choosing the defaults

The workloads are in `bench/sparse/` (its README lists them), timed and
measured by `bench/sparse/sweep.sh` over a grid: D ∈ {1/2, 1/4, 1/8, 1/16,
1/32}, D' ∈ {2D, 4D}, W ∈ {8 KiB, 64 KiB, 256 KiB, 1 MiB}, P = 16 MiB (and
1 MiB, 128 MiB at one point), both ends, and V8, SpiderMonkey and JSC.
`bench/sparse/pareto.py` scores each setting by the geometric mean of
its time and peak RSS as multiples of the best setting per workload, and
lists the Pareto front. `bench/sparse/results.tsv` is the run the defaults
come from (cloison 45d00fe, 3 runs a point, host load about 30 of 96).

- **W decides most.** Every setting on the front has W = 8 KiB. With a
  larger W, `many` (2,000 arrays of ten elements and one key 20,000 past
  them) keeps 160 KB of holes per array: 230-250 times slower and 45-60
  times larger than the best.
- **The front:** `1/32,1/16,8k` (time 1.18, memory 1.19), `1/16,1/8,8k`
  (1.23, 1.12) and `1/8,1/4,8k` (1.41, 1.11). The middle one is the
  default. Its worst cases are a fill from the end, `reverse` N=1e6, 2.9
  times always-dense's time (320 ms; V8 638), and `strided` K=16 at 2.1
  times the smallest memory. Against the engines' best per workload it is
  1.56 times the time and 1.06 times the memory, faster than all three on
  `prefix-far`, `reverse` and `random`.
- **P** barely moves it: with D' = 1/8 a far preallocation goes dense once
  an eighth is filled. 1 MiB doubles `prealloc` N=1e6; 16 MiB to infinity
  are level.
- The first sweep's memory was the collector's, not the arrays': compiled
  code growing an array by index reached no safepoint, so the collection
  asked for waited (fixed in 45d00fe: `strided` K=64 dense took 13 GB).

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

The helpers are `array::skip` (and `skip_back`, going down), over
`far::next_key` and `far::prev_key`. Inside the dense prefix a loop steps
as before; past it, `skip` returns the smallest far key at `k` or above (or
the length), found in a sorted copy of the keys kept on the VM. A store
rehashed into, or a `mutations` count moved since (a callback added a key),
sorts them again; a key deleted since is still in the copy, and the loop's
HasProperty finds it absent. The conditions are checked again at each step,
in constant time. An array with shape-named index keys (`SPARSE`) isn't
skipped.

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

## Status

Implemented: 6fb8658 (store, flag, policy, settings, compiled-code
guards), 4939098 (builtins over present elements, the density count at
reallocation, the preallocation floor), 45d00fe (the collection before a
keyed store from compiled code), and the defaults from the sweep. test262:
19 more pass (the 18 "too large for this engine" ones and
`S15.4_A1.1_T10`, an oom), none fewer. 209f9f8 fixed, on the way, compiled
appends to a sealed or non-extensible array whose elements had changed
type, which the slow shape relies on.

Not done: plain objects' far keys stay shape names (`SPARSE`), `reverse`,
`shift`, `unshift`, `splice`, `copyWithin` and `fill` still step through
every index (as the spec has `fill` and `copyWithin` do anyway).

## Open questions

- Overwrites in the prefix of a `FAR` array go to the runtime: the slow
  shape's `elems_code` fails every compiled store, in place too
  (`prefix-far` is still faster than the engines). A shape whose
  `elems_code` admits in-place stores but has no push cache would keep
  them compiled while appends still exit.
- A far store costs about 130 ns from compiled code (the runtime call, two
  probes, the length and density checks), against 6 ns dense: the fills
  that start far (`reverse`, `random`) are 2-3 times always-dense. A store
  that knows the key is new would save a probe.
- Large young cells are freed by minor collections, but a cell that
  survives one (a growing store, a growing array's elements) waits for a
  full collection.
