# Object model

Design for JS objects in toi. Goals, in order:

1. **DeltaBlue-class code runs fast**: class hierarchies, prototype methods,
   polymorphic calls, object graphs held in collections.
2. **Objects the outside world can't observe cost as little as possible,
   already in the interpreter.** `const o = {x: a, y: b}; return o.x + o.y`
   should run as `a + b`, and an object graph that never becomes externally
   visible shouldn't pay for the general heap.
3. **Dispatch stays as it is**: 3 instructions, and no extra work in handlers
   that don't deal with objects.

A note on the motivating example: with no declaration,
`obj = {x: a, y: b}` assigns a **global** in sloppy mode (and throws in
strict mode), so the object is externally visible (`globalThis.obj` after
the call). Everything below assumes `let`/`const`.

## Internal and external

An object is **external** if the outside world can observe it, and
**internal** otherwise. "Outside" means anything beyond the running program
fragment's own frames:

- global variables and the global object;
- objects handed to or obtained from the host (the embedder, I/O, timers,
  `postMessage`);
- native functions that keep a reference;
- values returned to the host;
- module exports, and anything else reachable from the outside.

It's **transitive**. An object stored into an external object becomes
external, and so does everything reachable from it. An object stored into an
internal object stays internal. Returning an object to a caller, passing it
as an argument, or storing it into an array are *not* escapes unless that
caller, callee or array is itself external.

**The invariant: no external object ever points to an internal one.**
Internal objects may point to external ones (a constraint pointing to the
global `Strength.REQUIRED` is fine).

Execution is optimistic: **everything is allocated internal**, and becomes
external lazily, when the program makes it observable. An allocation site
whose objects nearly always become external gets that as feedback, and
allocates external directly.

## Three representations

| kind | where | when | field access | memory |
|---|---|---|---|---|
| **virtual** | VM registers, one per field | the bytecode compiler proves the object never leaves its function | a register read (zero) | none |
| **internal** | the internal heap | the default for every allocation | inline cache | bump allocation; collected from frame roots only; freed in bulk |
| **external** | the external heap | the object became observable | inline cache | general GC |

Internal and external objects have the **same layout and shapes**, so the
same inline caches handle both, and no handler checks which kind it has.
Only stores into external objects check (the externalization barrier
below). A virtual object is an internal object the compiler took apart, with
a descriptor (the VirtualObject) saying how to rebuild it.

### Virtual objects: static scalar replacement

For an allocation site whose object provably never leaves its function, and
whose shape is static (an object literal or a known class constructor), the
bytecode compiler doesn't allocate at all:

```text
const o = {x: a, y: b};      Ldar a; Star o.x     ; o.x, o.y are registers
return o.x + o.y;            Ldar b; Star o.y
                             Ldar o.x; Add o.y; Return
```

After register coalescing, that is `Ldar a; Add b; Return`. The interpreter
runs `a + b`, and so does every JIT tier, for free.

"Leaves its function" is stricter than "becomes external": stored into any
object, returned, passed to a call the compiler can't see into, captured by
a closure, thrown, used with a computed key or by an operation that needs a
real object (`Object.keys`, `instanceof`, `for…in`, `delete`, getters and
setters). Identity is static: a fresh object is `===` only to itself.

**On a rare path, the object is rebuilt rather than given up.** At the use,
the compiler emits `Materialize o`, carrying a VirtualObject descriptor
(shape plus the registers holding the fields), which builds a real
*internal* object. After that, that path uses the real object. Where
materialized and virtual paths merge, the simple rule is to materialize on
every incoming edge; Graal's partial escape analysis tracks state per path
and is an option for later.

This is what the optimizing tiers of V8 (Turbofan's escape analysis,
captured objects at deoptimization) and JSC (FTL's allocation sinking) do,
moved to bytecode generation, where everything needed is statically known.

### Internal objects: the internal heap

Every other allocation goes to the internal heap: bump allocation, the same
header and slots as an external object.

- **No barriers between internal objects.** Stores into internal objects,
  returns and argument passing never check anything. In DeltaBlue's hot
  loops, stores go into internal objects (variables, constraints, plans), so
  nothing is checked there.
- **Collection needs only the frames as roots.** By the invariant, nothing
  external points in, so collecting the internal heap traces from the stack
  (interpreter registers, the accumulator, JIT slots written back at the
  safepoint) and never scans the external heap. No remembered set either.
- **Bulk free.** When no frame references any internal object, for example
  when a call from the host returns a number, the whole internal heap is
  dropped in O(1). For DeltaBlue that's every call of `deltaBlue()`: its
  objects die together and are never traced.
- **A function-local object** (it never leaves its frame, but isn't
  virtual, e.g. passed by reference to a known callee) can be bump-allocated
  in the frame and freed at frame exit, as a refinement for later.

Explicit dropping doesn't generalize: within an internal graph, aliasing
makes "who frees this?" undecidable (DeltaBlue destroys constraints while
other objects may still reference them). The frame-rooted collector is the
cheap replacement.

### Externalization

The only barrier is on operations that make a value observable:

- storing into an external object (a store into a global is a store into the
  global object),
- passing to the host, or to a native function that keeps it,
- returning to the host.

The check: **the target is external and the value is an internal object.**
It's two header-bit tests, on stores only. In DeltaBlue's hot loops it never
fires: `planner.currentMark = …` stores a number.

When it fires, the value is **externalized transitively, in place**: set the
external bit on it and on every internal object reachable from it, at a cost
proportional to that subgraph. No copying and no reference rewriting happen
then. The next internal collection, which traces anyway, moves
external-marked objects to the external heap and updates references, as a
copying collector does.

The **external bit** goes in the object header (a bit in the shape word, or
a per-page flag), not in references. A tag bit in references would make
every dereference strip it, including every inline-cache hit, which is
DeltaBlue's hot path.

**Precedent:** thread-local heaps (Steensgaard 2000; Domani et al. 2002)
use the same invariant and barrier, with "external" playing "shared": local
heaps are collected without synchronization, and an object becomes shared
when stored into a shared object.

## When to flatten

Being internal makes flattening *allowed*; whether it *pays* is a separate,
cost-driven decision for the bytecode compiler. Two transformations go by
that name:

- **A. Scalar replacement:** the object disappears; its fields become VM
  registers (the virtual kind).
- **B. Compact layout:** the object still exists in the internal heap, but
  with a layout the compiler fixes: static offsets with no inline-cache check
  when its type is known, a smaller header, unused fields dropped, uniquely
  owned children inlined.

### A. Scalar replacement

**What it buys.** No allocation: about 8–10 interpreter instructions, and
internal-heap pressure. Each field access becomes a register access instead
of an inline cache (about 7 instructions saved per access in the
interpreter). It also exposes the values to the compiler: copy propagation
turns `o.x` into `a`, unused fields vanish, and in the JIT the fields can
take register homes.

**What it costs, and when it loses:**

- **Materialization on escaping paths.** If the object escapes on a path
  that runs often, it's allocated anyway, plus the field copies and the
  extra bytecode. Flattening only pays when escaping paths are cold.
- **Frame size and register pressure.** Every field is a VM register. In the
  interpreter that is just frame size, but in the JIT, fields compete for
  slots (6 GPRs on x86-64, 12 on arm64) with everything else. Large objects
  (a 40-field configuration literal) gain little and crowd out hotter values.
- **Calls.** An internal object passed to a function the compiler doesn't
  inline must exist. Keeping it flat across calls needs either bytecode-level
  inlining, or callee variants taking fields as arguments. That duplicates
  code, so it's worth it only for small hot callees (getters, `add(p, q)`).
  The same goes for returning objects (`return {q, r}`), which needs
  multi-value returns or inlining.
- **Several allocation sites merging** (`const p = c ? {x: 1} : {x: 2}`).
  With the same shape, the fields merge per register at the join (Graal's
  partial escape analysis does this); with different shapes, materialize.
- **Shape not static at every use.** Conditionally added properties,
  `delete` or computed keys: either track presence per field, or don't
  flatten. Not flattening is the sensible default.
- **Allocation in a loop** while the previous instance is still referenced
  (`prev = cur; cur = {…}`): two live instances of one site need two
  register sets. Only flatten when at most one instance is live at the
  allocation.
- **Closures capturing the variable** put the object in a heap context:
  that's leaving the function, so no flattening unless the closure is
  inlined too.

**The rule for the bytecode compiler,** which runs before any profile
exists. Flatten a site when all of these hold:

1. every use is a field read or write with a constant key, a `===` (which
   folds), or a call the compiler inlines;
2. the shape is the same at every use, or all merging sites share a shape;
3. no path leaves the function, or only paths that are statically cold:
   `throw`, `catch` blocks, assertion and error branches;
4. at most one instance of the site is live at the allocation (for sites in
   loops);
5. at most K fields (K around 8 to 16).

Everything else stays an internal heap object, which is still cheap: bump
allocation and no barriers. Decisions that need profiles, such as "this
escaping path is cold in practice", belong to the JIT, which has the
feedback, exits and VirtualObjects to back out.

### B. Compact layout

Internal objects share the external layout so that inline caches serve both
and externalization can happen in place. A different internal layout
undoes both: every access site must handle both layouts, and
externalization has to copy and convert. So compact layouts are only for
sites the analysis proves **never externalize**, and whose type is known at
their access sites:

- **Static offsets with no shape check**, where the type at an access is
  certain. Polymorphic sites (DeltaBlue's constraints) keep inline caches.
- **A header of just the shape word**: a fixed-shape object never needs the
  out-of-line properties or elements pointers. That's 16 bytes per object,
  about 20% of a DeltaBlue `Variable` (7 fields).
- **Dropping fields never read.** Nothing outside can observe them, so a
  field written but never read (on any path) can go, e.g. debugging
  names.
- **Object inlining** (Dolby & Chien): a child object uniquely owned by its
  parent (`rect.topLeft = {x, y}`, never aliased or compared) is laid out
  inside the parent. That saves a header, a pointer and an indirection.
  References to it would become (parent, offset) pairs, so it's only for
  children never referenced on their own.

These need a whole-program view (every access and every escape known),
which a single script compiled at once gives. They're a later step than A,
and the first candidate to measure on DeltaBlue.

### Patterns

| pattern | verdict |
|---|---|
| object literal used locally (`const o = {x: a, y: b}`) | **A**: becomes registers |
| accumulator object updated in a loop (`acc.sum += x`) | **A**: fields in registers, JIT slot homes; a big win |
| options object passed to a large function | internal heap object; flatten only if the callee is inlined |
| iterator results `{value, done}`, destructuring `const [a, b] = f()` | **A** once the producer is inlined (`for…of` over arrays, generators: bytecode-level inlining matters) |
| small value objects through small helpers (`add(p, q)`) | **A** with bytecode-level inlining or field-passing callee variants |
| object returned from a non-inlined function | internal heap object |
| conditionally shaped objects, `delete`, computed keys | internal heap object, inline caches |
| large literals (dozens of fields) | internal heap object |
| objects compared by identity with others, used as `Map` keys | internal heap object (stored into the `Map`) |
| long-lived graphs in collections (DeltaBlue) | internal heap objects; **B** where types are known; bulk free |
| uniquely owned sub-objects (`rect.topLeft`) | **B**: object inlining |
| linked lists and trees | internal heap objects |

### Measuring before deciding

The thresholds above (K, which paths count as cold, what to inline) should
come from measurement: classify allocation sites in DeltaBlue, Richards and
the rest of Octane, plus real-world code, by which rules they meet, and
count dynamic allocations and field accesses per class. That's a cheap
first experiment once the front end parses them, and it tells the bytecode
compiler where flattening matters.

## External objects, shapes and inline caches

The general heap, and the standard design of V8, JSC and SpiderMonkey, which
internal objects share:

- **Header:** a pointer to the object's **shape** (hidden class, V8's map,
  JSC's structure), then pointers to out-of-line properties and to elements.
- **In-object slots** sized by the constructor's slack tracking, so
  `this.x = …` in a constructor writes a fixed offset.
- **A shape** maps names to slot offsets and attributes, links to the
  prototype, and records transitions, so objects built the same way share
  shapes.
- **Prototype chains** cached through validity cells: a prototype method
  load checks the receiver's shape and one cell, not the whole chain.
- **Dictionary mode** for objects used as maps; **arrays** with elements
  kinds (packed int, double, object; holey).

Property bytecodes carry a **feedback slot**:

```text
GetNamed r, name, slot     acc = r.name
SetNamed r, name, slot     r.name = acc          (externalization check here)
CallProperty r, name, slot, args   acc = r.name(args), with r as this
Construct r, args, slot
```

The slot caches what was seen: monomorphic (one shape and an offset),
polymorphic (up to 4, covering DeltaBlue's constraint hierarchy at
`c.execute()`-style sites), or megamorphic (a global lookup cache). The hit
path is: load the receiver's shape, compare it to the cached one, load the
slot, dispatch. That's about 7 or 8 instructions plus dispatch. A miss goes
to a cold runtime path.

These slots are the **type feedback** the JIT needs. With objects, the types
of the values at entry say nothing about the shapes a loop will see, but
the inline caches do. That's why tiering is the default.

## The JIT

- **Property-access stencils** with the shape check and offset patched in as
  holes, fed by inline-cache feedback; a shape mismatch leaves through the
  existing exit stubs.
- **Scalar replacement in the JIT** (later) uses VirtualObjects in its exit
  metadata. Unlike V8, it rebuilds them as *internal* objects, a bump
  allocation that is never traced unless still live at the next collection.
- **Collections and externalization are safepoints.** JIT code caches VM
  registers in machine registers, so the slots have to be written back
  where the collector can see and update them, as an exit already does.

## DeltaBlue, in its own code

A constraint solver: `Variable`s, a `Constraint` hierarchy
(`UnaryConstraint` → `StayConstraint`, `EditConstraint`; `BinaryConstraint`
→ `ScaleConstraint`, `EqualityConstraint`), `Strength` constants, `Plan`,
`Planner`, and an `OrderedCollection` that wraps a JS array. The excerpts
below are quoted from Octane's `deltablue.js` (Copyright 2008 the V8 project
authors, Copyright 1996 John Maloney and Mario Wolczko, GPL-2.0-or-later;
not vendored here). Line numbers refer to that file.

### What is external

Everything reachable from a global. These are setup-time objects, built
once per load:

```js
// deltablue.js:26
var DeltaBlue = new BenchmarkSuite('DeltaBlue', [66118], [
  new Benchmark('DeltaBlue', true, false, 4400, deltaBlue)
]);
// deltablue.js:49
Object.defineProperty(Object.prototype, "inheritsFrom", {
  value: function (shuper) {
    function Inheriter() { }
    Inheriter.prototype = shuper.prototype;
    this.prototype = new Inheriter();
    this.superConstructor = shuper;
  }
});
// deltablue.js:137
Strength.REQUIRED        = new Strength(0, "required");
// deltablue.js:335
var Direction = new Object();
Direction.NONE     = 0;
```

- `new Inheriter()` is stored into a constructor's `prototype`, `new
  Strength(…)` into a property of the `Strength` constructor, and `new
  Object()` into a global. Each externalizes at its store, but each is a
  leaf with nothing internal behind it, so the transitive walk is one
  object. These sites run once. Allocation-site feedback would make them
  allocate external directly, but it's not worth it for a single run.
- **Advantage:** none of this matters for the hot code. Instances point
  *to* these objects (`this.strength = Strength.REQUIRED`), which the
  invariant allows, and the store needs no barrier: the target is internal.
- **Issue: constants behind property loads.** `Direction.FORWARD` and
  `Strength.REQUIRED` are reloaded in every hot method:

  ```js
  // deltablue.js:414
  BinaryConstraint.prototype.output = function () {
    return (this.direction == Direction.FORWARD) ? this.v2 : this.v1;
  }
  ```

  That's a global load plus a named load (two inline caches), around 15
  interpreter instructions for what is the constant `1`. This is unrelated
  to internal/external. It needs **constant-property tracking** on external
  objects: a property written once and never again is marked constant in its
  shape, and a dependent compiler folds it with a watchpoint (V8's const
  field tracking, JSC's watchpoints). The bytecode compiler can't fold it,
  since nothing stops a later script from writing `Direction.FORWARD = 7`.
  The JIT can.
- **Issue: `inheritsFrom` lives on `Object.prototype`.** Every prototype
  chain ends there, so a property miss anywhere walks to it. Validity cells
  make hits cheap. A miss (e.g. `out.determinedBy` read before it's
  written) would walk the chain each time, but DeltaBlue's constructors
  initialize every field they read, so misses don't happen on hot paths.

### The global `planner`, and why it doesn't drag the graph out

```js
// deltablue.js:878
var planner = null;
// deltablue.js:801  (chainTest)
  planner = new Planner();
// deltablue.js:573
function Planner() {
  this.currentMark = 0;
}
```

The planner is stored into a global, so it's **external** from the start.
Constraint methods call it with themselves as the argument:

```js
// deltablue.js:163
Constraint.prototype.addConstraint = function () {
  this.addToGraph();
  planner.incrementalAdd(this);
}
```

- **Advantage: passing to an external object's method is not an escape.**
  `incrementalAdd(this)` binds `c` in a frame. Externalization happens only
  if the planner *stores* the constraint into itself, and it never does: all
  its state is `currentMark`, a number. Its worklists (`todo`, `unsatisfied`)
  are locals. Under an "anything touching an external object escapes" rule
  (or V8's escape analysis, which gives up on non-inlined calls), the whole
  graph would be lost here, through the hottest call in the program.
- **The cost:** each `newMark()` does `++this.currentMark`, a store into an
  external object, so it runs the barrier: is the target external (yes), is
  the value an internal object (no, a number). The second test fails fast,
  two header-bit checks, and only on `SetNamed`.
- **Issue: `planner` is a mutable global.** `chainTest` and `projectionTest`
  each assign it, and Octane calls `deltaBlue()` many times. A JIT that folds
  `planner` as a constant would be invalidated twice per run. It should
  treat `planner` as a mutable global cell whose *shape* is stable: load the
  cell, skip the shape check.
- **Fragility:** one innocent edit, e.g. `Planner.prototype.incrementalAdd`
  doing `this.last = c` for debugging, externalizes `c`, its variables,
  their `OrderedCollection`s, every constraint reachable from them, and so
  the whole graph, on the first call. That's correct, but it's a cliff. The
  fix is allocation-site feedback: after the first walk, `Variable` and the
  constraint constructors allocate external directly, and the program runs
  like it would in V8. **It must never be worse than the external-only
  baseline by more than that one walk.**

### The graph is built by constructors, and stays internal

```js
// deltablue.js:805
  for (var i = 0; i <= n; i++) {
    var name = "v" + i;
    var v = new Variable(name);
    if (prev != null)
      new EqualityConstraint(prev, v, Strength.REQUIRED);
    …
    prev = v;
  }
```

`new EqualityConstraint(…)` is used for its side effects only: the result
is dropped. The constructor calls `this.addConstraint()`, which pushes
`this` into both variables' collections:

```js
// deltablue.js:384
BinaryConstraint.prototype.addToGraph = function () {
  this.v1.addConstraint(this);
  this.v2.addConstraint(this);
  this.direction = Direction.NONE;
}
// deltablue.js:554
Variable.prototype.addConstraint = function (c) {
  this.constraints.add(c);
}
```

- **Advantage:** the constraint escapes the constructor into a
  `Variable`'s collection, and the variable into a local. Under the
  transitive definition, neither is external: no barrier on any of these
  stores, bump allocation, and when `deltaBlue()` returns `undefined`, the
  whole graph is garbage without being traced.
- **Bulk free, made precise.** The harness (Octane's `base.js`) calls
  `deltaBlue()` from JS, with live harness frames below. They hold only
  external objects (the `Benchmark` is stored in `BenchmarkSuite.suites`),
  but proving that needs a scan. A cheaper rule covers this case: **a call
  whose arguments, receiver and closure context hold no internal object,
  and that returns no internal object, leaves nothing it allocated
  internally reachable.** By the invariant, the callee can't reach anything
  internal from before the call, and anything it made reachable from
  outside was externalized. So the bump pointer can be reset to its value at
  entry. `deltaBlue()` takes no arguments, its context is the global scope,
  and it returns `undefined`, so each iteration of the benchmark is a free
  region. It doesn't apply to `change(v, newValue)`, whose `v` is
  internal. Checking the condition costs a header test per argument at
  call sites that opt in. A collection during the call must then respect
  the region mark (collect only above it, or forget the mark).
- **Issue: no flattening here.** Every graph object is reachable from other
  heap objects, and many are created in loops while earlier ones stay live
  (`prev = v`). Rules 3 and 4 rule out A. Graph objects are internal heap
  objects, as they should be.

### Same methods, different shapes

```js
// deltablue.js:344
function BinaryConstraint(var1, var2, strength) {
  BinaryConstraint.superConstructor.call(this, strength);  // this.strength
  this.v1 = var1;
  this.v2 = var2;
  this.direction = Direction.NONE;
  this.addConstraint();
}
// deltablue.js:458
function ScaleConstraint(src, scale, offset, dest, strength) {
  this.direction = Direction.NONE;
  this.scale = scale;
  this.offset = offset;
  ScaleConstraint.superConstructor.call(this, src, dest, strength);
}
```

An `EqualityConstraint` gets fields in the order `strength, v1, v2,
direction`, while a `ScaleConstraint` gets `direction, scale, offset,
strength, v1, v2`. Shared methods such as `BinaryConstraint.prototype.output`
(above) load `this.direction` and `this.v1` at **different offsets**
depending on the receiver.

- **Issue:** the inline caches in every `BinaryConstraint` method are
  polymorphic (two shapes in `projectionTest`), and B's static offsets
  don't apply.
- **Tradeoff, specific to internal objects:** property order is observable
  only through enumeration (`for…in`, `Object.keys`, `JSON.stringify`,
  spread). DeltaBlue never enumerates instances. So for internal objects,
  the compiler may **assign slots by class hierarchy** (a field declared by
  `BinaryConstraint` at the same offset in every subclass), keeping the
  enumeration order in the shape apart from the slot order. Externalization
  stays in place: the shape still describes the object. This is the most
  valuable B transformation for DeltaBlue, and it needs the whole-program
  view: no enumeration of these objects anywhere, which a single script
  shows.
- **`superConstructor.call(this, …)`** goes through `Function.prototype.call`
  on every construction. It needs a fast path that turns `f.call(r, args…)`
  into a direct call with `r` as `this`, since it's on the construction path
  of every constraint. In `ScaleConstraint.prototype.addToGraph` it's even
  `ScaleConstraint.superConstructor.prototype.addToGraph.call(this)`: three
  dependent loads before the call.
- **The calls are polymorphic, but mildly.** `c.execute()` in
  `Plan.execute` sees an `EditConstraint` plus `EqualityConstraint`s (in
  `chainTest`) or `ScaleConstraint`s (in `projectionTest`), so 2-way
  polymorphic inline caches are enough. `chooseMethod`, `isSatisfied` and
  `output`, called from `Planner` on every constraint kind, see 4 shapes.

### Wrappers: where flattening would pay

`OrderedCollection` is a one-field wrapper around an array, and its methods
are one-liners:

```js
// deltablue.js:59
function OrderedCollection() {
  this.elms = new Array();
}
OrderedCollection.prototype.add = function (elm) {
  this.elms.push(elm);
}
OrderedCollection.prototype.size = function () {
  return this.elms.length;
}
OrderedCollection.prototype.removeFirst = function () {
  return this.elms.pop();      // despite the name, LIFO
}
```

The worklists in `Planner` are local wrappers:

```js
// deltablue.js:694
Planner.prototype.addPropagate = function (c, mark) {
  var todo = new OrderedCollection();
  todo.add(c);
  while (todo.size() > 0) {
    var d = todo.removeFirst();
    if (d.output().mark == mark) {
      this.incrementalRemove(c);
      return false;
    }
    d.recalculate();
    this.addConstraintsConsumingTo(d.output(), todo);
  }
  return true;
}
// deltablue.js:741
Planner.prototype.addConstraintsConsumingTo = function (v, coll) {
  var determining = v.determinedBy;
  var cc = v.constraints;
  for (var i = 0; i < cc.size(); i++) {
    var c = cc.at(i);
    if (c != determining && c.isSatisfied())
      coll.add(c);
  }
}
```

- **Advantage: A applies to the wrapper.** `todo` never leaves
  `addPropagate` except as the argument to `addConstraintsConsumingTo`,
  which is small and only calls `coll.add`. Inline `add`, `size`,
  `removeFirst` and `addConstraintsConsumingTo` at bytecode level, and the
  `OrderedCollection` becomes one VM register holding `elms`. The *array*
  stays a heap object, since its length is dynamic. That saves one
  allocation per call and one dependent load per access (`todo.elms`), in
  the solver's inner loop.
- **The limit: method calls are dynamic.** `todo.add(c)` is a
  `CallProperty` on `OrderedCollection.prototype.add`, which a later script
  could replace. The bytecode compiler can inline it only with the
  whole-program view (no writes to `OrderedCollection.prototype` after
  setup), or with a guard (check the prototype's validity cell, fall back
  to materializing `todo`). This is the "calls" cost from A, and it's the
  common case in DeltaBlue-style code: almost every use is a method call.
- **Issue: the same wrapper is not always local.** `removePropagateFrom`
  *returns* `unsatisfied`, and `makePlan` consumes its caller's `sources`
  (`var todo = sources;` then `todo.removeFirst()`). Those stay internal
  heap objects, unless the caller is inlined too.
- **A per-site decision:** the same class is flattened at one allocation
  site (`addPropagate`'s `todo`), stays a heap object at another
  (`removePropagateFrom`'s `unsatisfied`), and is inlined into its owner at
  a third (below). Flattening decisions belong to allocation sites, not
  classes.

### Owned children: object inlining

```js
// deltablue.js:540
function Variable(name, initialValue) {
  this.value = initialValue || 0;
  this.constraints = new OrderedCollection();
  this.determinedBy = null;
  this.mark = 0;
  this.walkStrength = Strength.WEAKEST;
  this.stay = true;
  this.name = name;
}
// deltablue.js:760
function Plan() {
  this.v = new OrderedCollection();
}
Plan.prototype.execute = function () {
  for (var i = 0; i < this.size(); i++) {
    var c = this.constraintAt(i);
    c.execute();
  }
}
```

- **Advantage:** each `Variable` and `Plan` uniquely owns its
  `OrderedCollection`, which is never reassigned or compared. Laying it out
  inside its owner (B, object inlining) turns `v.constraints.elms.length`
  into `v.elms.length`: one header and one dependent load less, in the loops
  that walk constraints. `Plan.execute`, the hottest loop of `chainTest`
  (100 runs over a 101-constraint plan), goes from `this.v.elms[i]` behind
  three calls to one load once inlined.
- **Issue: the child is referenced on its own.** `var cc = v.constraints;`
  (above) takes a reference to it. Object inlining is still possible if
  every use of `cc` inlines (`cc.size()`, `cc.at(i)`), so it can be
  rewritten to `v`-relative accesses. If `cc` were passed to a non-inlined
  function, it would need a real object. The analysis has to check every
  read of the field, not just the allocation.
- **Dropping a field: `name`.** `Variable.name` is written and never read
  anywhere in the program. Since the variables are internal, nothing can
  observe it, so the field goes, and with it `var name = "v" + i;`
  (concatenating a string and a number has no side effects): 101 string
  allocations per `chainTest`, 200 per `projectionTest`. `Strength.name` is
  never read either, but `Strength` objects are external, so it stays.

### Numbers in fields

```js
// deltablue.js:490
ScaleConstraint.prototype.execute = function () {
  if (this.direction == Direction.FORWARD) {
    this.v2.value = this.v1.value * this.scale.value + this.offset.value;
  } else {
    this.v1.value = (this.v2.value - this.offset.value) / this.scale.value;
  }
}
```

`Variable.value` holds int32 values in this benchmark (the division,
`(1050 - 1000) / 10`, is exact), but nothing guarantees that statically.
This is the slot-level version of the JIT's unboxed int32 work: a field
representation in the shape (int32, double, tagged), generalized on a
store that doesn't fit, like V8's field representations. With NaN-boxing,
an int32 or double in a field is already unboxed, so it's only worth doing
to let the JIT skip type checks on loads.

### Summary for DeltaBlue

| site | kind | why |
|---|---|---|
| `Strength.*`, `Direction`, prototypes, `Inheriter` instances, the `Benchmark` | external | stored into globals at setup; leaves, cheap to externalize |
| `planner` | external | a global; holds only a number, so no internal object follows it out |
| `Variable`, constraints | internal heap | escape into each other's collections, not into anything external |
| `Variable.constraints`, `Plan.v` | internal, inlined into owner (B) | uniquely owned; every use inlines |
| `todo` in `addPropagate`, `removePropagateFrom` | virtual (A) for the wrapper; its array internal | local, used through small inlinable methods |
| `unsatisfied`, `edits`, `sources`, plans | internal heap | returned or passed to non-inlined calls |
| everything internal after `deltaBlue()` returns | freed as a region | the call takes and returns no internal object |

The biggest wins in DeltaBlue aren't from flattening. They're from the
internal heap (no barriers, no tracing, region free), consistent slot
offsets across the hierarchy, object inlining of the collections, and fast
polymorphic calls. A buys little beyond the worklist wrappers, but it
doesn't have to: it targets local temporaries, which DeltaBlue's style
barely has. Richards and real-world code (iterator results, options
objects, `{x, y}` points) are where A should be measured.

## Risks and open questions

- **What counts as external** has to be complete, or the invariant breaks
  silently: every host API, every native that retains, every global path
  (including `globalThis` properties, prototype objects reachable from
  globals, closures stored in them). Mistakes there are memory-safety bugs
  (an external object pointing into a freed internal heap), so the barrier
  should sit at a few choke points (the store paths and the host boundary)
  and be fuzzed, e.g. with a debug mode that verifies the invariant at each
  collection.
- **Externalizing a large graph** costs time proportional to it. That's fine
  if rare, and allocation-site feedback catches the sites where it isn't.
- **Prototype objects** are external but written during setup (methods
  installed), and instances point to them, which is allowed. An
  instance stored *into* a prototype (a cached singleton, say) externalizes.
- **Closures** capture environments: a closure stored externally
  externalizes its context and what it reaches.
- **Generators and async functions** suspend frames into objects. Those are
  internal unless stored externally; the internal collector must trace
  suspended frames held by internal objects.
- **`eval`, `with`, `arguments`, debuggers** defeat static analysis: no
  virtual objects in such functions. Internal objects still work.
- **GC.** Both heaps can start simple: copying for the internal heap
  (it moves external-marked objects out), non-moving mark-and-sweep for the
  external heap; generational later.

## Plan

1. **Objects and shapes**, one heap first with the external bit in the
   header but always set, plus a simple GC: get correctness right.
2. **Bytecodes with feedback slots** (`GetNamed`, `SetNamed`,
   `CallProperty`, `Construct`, `CreateObjectLiteral`), interpreter inline
   caches (monomorphic, polymorphic, megamorphic), function calls and
   frames. **A JS front end** to run real programs such as DeltaBlue.
3. **The internal heap**: allocate internal by default, the externalization
   barrier and transitive externalization, the frame-rooted collector, bulk
   free, an invariant-checking debug mode.
4. **Virtual objects**: escape analysis and scalar replacement in the
   bytecode compiler, `Materialize` with VirtualObject descriptors.
5. **JIT**: property-access stencils fed by inline caches, shape checks as
   exits, VirtualObjects at exits.

### Status

Steps 1 and 2 have started, without a front end: hand-written bytecode
for five microbenchmarks (`bench/bytecode/`) runs on
`src/object.rs` (objects, shapes, the heap), `src/vm.rs` (inline-cache
misses, closures, arrays, natives) and the interpreter (`src/interp.rs`).
In place:

- Shapes with shared transition trees, roots keyed by class, prototype and
  in-object capacity, and slack tracking per constructor.
- Monomorphic inline caches for `GetNamed`, `SetNamed` (including cached
  transitions to in-object slots), `LdaGlobal` and `StaGlobal`, holding the
  shape and the location. Prototype loads cache the holder too. Prototypes
  get unique shapes, and adding a property to one bumps a global
  epoch that every cached prototype load checks. That is simpler than
  per-prototype validity cells.
- Frames on a VM stack, entered and left by tail calls; `Construct` with
  the `this`-or-object rule; natives (`Array.prototype.push`).
- The external bit, always set.

Step 5 has started too. Hot call-free loops in VM frames tier up into the
JIT (on-stack replacement from the interpreter). Property accesses compile
to stencils fed by the inline caches, with shape guards as exits. The JIT's
type lattice knows objects of a known shape (`Obj(shape)`), so a guard
that passed, or a shape seen at entry, isn't checked again. Details and
numbers: `docs/future-work.md`, "Property access in the JIT, and shape
types".

Still missing from steps 1–2: a collector (the heap is an arena freed with
the VM), polymorphic and megamorphic caches, a cache for array `length`,
property attributes, strings, sloppy-mode `this`, exceptions (`try`),
closures over variables, and the front end.
