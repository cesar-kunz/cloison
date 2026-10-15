# Benchmarks

Small JS programs for validating and benchmarking toi's JS-to-bytecode
compiler, interpreter and JIT.

## `micro/`: one pattern per file

Each file isolates one pattern of object-oriented JS: code idioms, the
object model, and the allocation and escape behaviour discussed in
[docs/object-model.md](../docs/object-model.md).

### Conventions

- **A plain script, with no host calls**: no `console`, no `print`, no
  `alert`. The result is the script's **completion value**, the value of its
  last expression statement (`main();`). That's standard JS: Node returns it
  from `vm.runInNewContext`, and toi returns it from running a script.
- **The result is a number**, a checksum of the work, so a wrong
  optimization changes it.
- **Hot code lives in functions.** Files that test globals
  say so.
- **ES5 only** (typed arrays aside, as in Octane): `var`, `function`, prototypes,
  `new`, `.call`, arrays, `switch`, `do`/`while`. No classes or arrow functions.
- **Header comments:**

  ```js
  // <what it does>
  // toi: <what it tests in toi's object model> (some files)
  // features: loop, call, object, ...
  // expect: 12345
  ```

  `features` lists the language features the file needs, from a fixed
  vocabulary (`FEATURES` in `run.mjs`): loop, double, string, switch, call,
  recursion, closure, object, new, proto, call-method
  (`Function.prototype.call`), array, global, null, define-property, for-in,
  throw, math, typeof, instanceof, arguments, literal, typed-array, getter.
  `--features` selects the files a front end in progress can already
  handle.

### Running

```sh
node bench/run.mjs                        # check every file against `expect`
node bench/run.mjs --time --reps 21       # plus median time per file
node bench/run.mjs --features loop,call   # only files needing just these
node bench/run.mjs --update               # rewrite `expect` from Node (the reference)
node bench/run.mjs --engine "target/release/toi" bench/micro/0*.js
```

- `--engine CMD` runs `CMD file` and compares the number it prints. toi's
  driver should accept either a `.js` file (parsed and compiled to bytecode
  on the fly) or a bytecode file from the bytecode compiler, so both paths
  are checked against the same expectations: compile each file ahead of time
  and run the output with `--engine` too.
- Node timings are only indicative. Each file runs in a fresh `vm` context
  (so `Object.prototype` changes don't leak between files), and Node
  accesses globals in such a context slowly, which shows in the
  files that use globals (10–16, 28–32, 50–53).

- `scripts/sizes.sh` reports the size of the runtime, of the stencils,
  and of the code the JIT generates for each listing in
  [bytecode/](bytecode/); `--record` appends them to
  [sizes.tsv](sizes.tsv), by commit, and every run shows the change since
  the last record. Speed comes first, but size is tracked: a small
  speedup for a large size increase is worth reconsidering.

- `scripts/tiers.sh` times every file in toi's interpreter, toi with
  the JIT, and V8 capped at each of its tiers (Ignition, Sparkplug,
  Maglev, Turbofan), as a table in `perf-history/tiers-<commit>.md`,
  timing both inside the process, startup excluded.
  V8's times are cached by host, V8 version and file contents, so only new
  or changed files are measured again.

- `scripts/vs-maglev.sh` compares the machine code toi's JIT generates
  for each file with V8's Maglev's (`--max-opt=2`), function by function
  (final code: the last compile of each entry), to find where toi's code
  is larger than it needs to be; `--asm FILE FN` prints both
  disassemblies of one function. Maglev's sizes are cached by Node
  version and file contents. Functions are matched by name and source
  position (`name:line:col`, which `TOI_JIT_LOG` prints). `--pieces`
  breaks toi's final code down per stencil family and per task of
  [docs/maglev-size-tasks.md](../docs/maglev-size-tasks.md);
  `--pieces --record` appends the task totals to
  [maglev-tasks.tsv](maglev-tasks.tsv), by commit, and every run shows the
  change since the last record.

- `scripts/jit-profile.sh <listing>` profiles where compiled code spends
  its cycles, per stencil piece (`TOI_PERF_MAP=1` makes the JIT name
  every piece in `/tmp/perf-<pid>.map`, which `perf report` reads too).

- Hand-written bytecode for some of these files is in
  [bytecode/](bytecode/), with the conventions a compiler should follow.
  toi runs it already (`--engine target/release/toi bench/bytecode/*.asm`).

### Files

| file | pattern |
|---|---|
| [01-int-loop.js](micro/01-int-loop.js) | Counted int32 loop summing the counter. |
| [02-int-compare-ternary.js](micro/02-int-compare-ternary.js) | Nested ternaries over int comparisons picking a direction. |
| [03-double-scale.js](micro/03-double-scale.js) | Linear map forward (x * scale + offset) and backward ((y - offset) / scale). |
| [04-int-to-double-field.js](micro/04-int-to-double-field.js) | A numeric field that holds int32 values, then doubles after an inexact division. |
| [05-logical-or-default.js](micro/05-logical-or-default.js) | `x \|\| 0` defaulting an argument that is often undefined. |
| [06-switch-int.js](micro/06-switch-int.js) | Dense int switch returning objects held in globals. |
| [07-string-concat-number.js](micro/07-string-concat-number.js) | Building names by concatenating a string and a number. |
| [08-do-while.js](micro/08-do-while.js) | do-while stepping through levels until a sentinel, with an inner counted loop. |
| [09-loose-equality.js](micro/09-loose-equality.js) | `==` / `!=` between objects, null and numbers. |
| [10-call-global-function.js](micro/10-call-global-function.js) | Calling a small top-level function from a loop. |
| [11-call-static-method.js](micro/11-call-static-method.js) | Calling a function stored as a property of a constructor ("static method"). |
| [12-call-this-static.js](micro/12-call-this-static.js) | A static method calling a sibling through `this` (the constructor). |
| [13-missing-arguments.js](micro/13-missing-arguments.js) | Calling with fewer arguments than parameters; the rest are undefined. |
| [14-parent-constructor-call.js](micro/14-parent-constructor-call.js) | Subclass constructors running the parent constructor via Function.prototype.call. |
| [15-super-method-call.js](micro/15-super-method-call.js) | Overriding method calling the parent's version: X.parent.prototype.m.call(this, ...). |
| [16-inner-function.js](micro/16-inner-function.js) | Creating an inner function per call, used as a constructor with a borrowed prototype. |
| [17-constructor-side-effects.js](micro/17-constructor-side-effects.js) | `new X(...)` whose result is discarded: the constructor registers the object elsewhere. |
| [18-constructor-calls-override.js](micro/18-constructor-calls-override.js) | A parent constructor calling a method that subclasses override, on an object still being built. |
| [19-cold-path-throw.js](micro/19-cold-path-throw.js) | Checks in hot code whose failure throws; they never fail. |
| [20-constructor-fields.js](micro/20-constructor-fields.js) | Constructor initializing seven fields of mixed kinds, then reading them back. |
| [21-monomorphic-get.js](micro/21-monomorphic-get.js) | Monomorphic field loads in a loop over same-shaped objects. |
| [22-monomorphic-set.js](micro/22-monomorphic-set.js) | Monomorphic field stores of ints, booleans and objects. |
| [23-prototype-method.js](micro/23-prototype-method.js) | Monomorphic method call through the prototype. |
| [24-inherited-method.js](micro/24-inherited-method.js) | Method found two levels up the prototype chain. |
| [25-polymorphic-get.js](micro/25-polymorphic-get.js) | Field loads at a site seeing two shapes that store the field at different offsets. |
| [26-polymorphic-call.js](micro/26-polymorphic-call.js) | Method call site seeing four receiver classes, each with its own implementation. |
| [27-shared-method-two-layouts.js](micro/27-shared-method-two-layouts.js) | One inherited method running on receivers of two subclasses with different field orders. |
| [28-define-on-object-prototype.js](micro/28-define-on-object-prototype.js) | A non-enumerable method installed on Object.prototype with Object.defineProperty, called on functions. |
| [29-property-miss.js](micro/29-property-miss.js) | Loading absent properties: the lookup walks to Object.prototype and yields undefined. |
| [30-new-object-constants.js](micro/30-new-object-constants.js) | An enum built with `new Object()` plus property stores, read in a loop. |
| [31-constant-props-in-methods.js](micro/31-constant-props-in-methods.js) | Methods comparing a field against a global constant property, called in a hot loop. |
| [32-identity-singletons.js](micro/32-identity-singletons.js) | Singletons stored on a constructor, compared by identity. |
| [33-null-fields.js](micro/33-null-fields.js) | Object-or-null fields: tested, overwritten and reset. |
| [34-chained-loads.js](micro/34-chained-loads.js) | Two- and three-step field load chains. |
| [35-chained-assignment.js](micro/35-chained-assignment.js) | Chained assignment storing one value into two objects' fields. |
| [36-boolean-and-chain.js](micro/36-boolean-and-chain.js) | && chains over boolean fields of three objects, result stored in a field. |
| [37-prefix-increment-field.js](micro/37-prefix-increment-field.js) | Pre-increment of a field, returning the new value. |
| [38-mixed-depth-polymorphic-call.js](micro/38-mixed-depth-polymorphic-call.js) | Call sites over four classes whose implementations live at different depths of the prototype chain. |
| [39-empty-polymorphic-methods.js](micro/39-empty-polymorphic-methods.js) | Calls to methods that do nothing or return a constant, at sites seeing three classes. |
| [40-array-push-length.js](micro/40-array-push-length.js) | Growing an empty `new Array()` with push and reading length. |
| [41-array-pop-worklist.js](micro/41-array-pop-worklist.js) | LIFO worklist: push seeds, pop one, push its successors, until empty. |
| [42-array-index-load.js](micro/42-array-index-load.js) | Indexed loads from a packed int array in a counted loop. |
| [43-array-compact-remove.js](micro/43-array-compact-remove.js) | Removing every occurrence of an element by in-place compaction, then popping the tail. |
| [44-array-scan-filter.js](micro/44-array-scan-filter.js) | Scanning an array of objects, calling a predicate method, collecting matches. |
| [45-collection-wrapper.js](micro/45-collection-wrapper.js) | A one-field wrapper class around an array, used only through one-line methods. |
| [46-polymorphic-elements-scan.js](micro/46-polymorphic-elements-scan.js) | Scanning a collection of objects of four shapes, calling a predicate on each, collecting the ones that pass. |
| [50-global-object-in-hot-method.js](micro/50-global-object-in-hot-method.js) | Methods reading a global variable that holds an object, and calling through it. |
| [51-global-reassigned.js](micro/51-global-reassigned.js) | A global reassigned between phases; methods keep reading it. |
| [52-global-store-number.js](micro/52-global-store-number.js) | Storing numbers into an object reachable from a global (an external object). |
| [53-top-level-loop.js](micro/53-top-level-loop.js) | Loop at script top level: every variable is a global property. |
| [54-global-reentrant-calls.js](micro/54-global-reentrant-calls.js) | Methods passing `this` to a method of a global object, which calls back into them, and they call the global again. |
| [60-local-worklist-wrapper.js](micro/60-local-worklist-wrapper.js) | A worklist wrapper allocated, filled and drained inside one function; it never leaves. |
| [61-worklist-passed-to-helper.js](micro/61-worklist-passed-to-helper.js) | A local worklist passed to a helper that only pushes into it. |
| [62-returned-collection.js](micro/62-returned-collection.js) | A function building a collection and returning it to its caller, which consumes it. |
| [63-consume-callers-collection.js](micro/63-consume-callers-collection.js) | A callee draining a collection its caller built (aliasing: `var todo = sources`). |
| [64-owned-child.js](micro/64-owned-child.js) | An object that uniquely owns a collection, accessed through the owner and through a local alias. |
| [65-dead-field.js](micro/65-dead-field.js) | A field written in the constructor and never read; its value is a fresh string. |
| [66-graph-by-constructors.js](micro/66-graph-by-constructors.js) | A chain of nodes and links built entirely by constructor side effects, then walked. |
| [67-arg-to-external-method.js](micro/67-arg-to-external-method.js) | Internal objects passed as arguments to a method of a global object that never stores them. |
| [68-externalize-graph-once.js](micro/68-externalize-graph-once.js) | A graph built internally, then made reachable from a global once, then used. |
| [69-region-per-call.js](micro/69-region-per-call.js) | A function that allocates many objects and returns only a number, called repeatedly. |
| [70-alloc-in-loop-prev-live.js](micro/70-alloc-in-loop-prev-live.js) | Allocating in a loop while the previous instance is still live (`prev = v`). |
| [71-short-lived-per-call.js](micro/71-short-lived-per-call.js) | Per-call temporaries: a request object, a one-element collection and a result built from them. |
| [80-plan-execute-copy.js](micro/80-plan-execute-copy.js) | Kernel: executing a plan (a collection of steps) over a chain, many times per input change. |
| [81-plan-execute-scale.js](micro/81-plan-execute-scale.js) | Kernel: a plan of linear-map steps run forward, then backward, with shared scale/offset cells. |
| [82-propagate-walk.js](micro/82-propagate-walk.js) | Kernel: worklist propagation of "max-of" ranks downstream through a graph, with marks. |
| [83-collect-and-readd.js](micro/83-collect-and-readd.js) | Kernel: collect unsatisfied items into a returned collection; re-add them lowest rank first. |
| [84-build-and-tear-down.js](micro/84-build-and-tear-down.js) | Kernel: build a graph by constructor side effects, then remove edges by in-place compaction. |
| [85-plan-extraction.js](micro/85-plan-extraction.js) | Kernel: extracting a plan from the input steps (inputs known, outputs marked, consumers queued), then running it. |
| [86-alternating-phases.js](micro/86-alternating-phases.js) | Kernel: two phases taking turns, each building fresh objects for shared functions (ints and two classes, then doubles and three). |
| [87-constraint-solver.js](micro/87-constraint-solver.js) | Kernel: a whole incremental constraint solver in DeltaBlue's style, on a chain and on a projection, repeatedly. |
| [90-recursive-fib.js](micro/90-recursive-fib.js) | Doubly recursive fib. |
| [91-mutual-recursion.js](micro/91-mutual-recursion.js) | fib over two mutually recursive functions. |
| [92-tail-recursion.js](micro/92-tail-recursion.js) | A tail-recursive sum, 5000 deep, run from a loop. |
| [93-constructor-loop.js](micro/93-constructor-loop.js) | A small constructor called in a loop, one field read back. |
| [94-constructor-and-native.js](micro/94-constructor-and-native.js) | A constructor and `push` in the same loop. |
| [100-int-bitops.js](micro/100-int-bitops.js) | Hash mixing with shifts, and, or, xor and unsigned shifts over int32 values. |
| [101-lcg-double-mod.js](micro/101-lcg-double-mod.js) | Park-Miller generator in doubles: products past int32, then `%` by a constant. |
| [102-math-builtins.js](micro/102-math-builtins.js) | Math.floor, sqrt, abs, min and max on doubles in a hot loop. |
| [103-float64-array-relax.js](micro/103-float64-array-relax.js) | Jacobi-style relaxation over a Float64Array grid (Navier-Stokes' lin_solve). |
| [104-byte-array-checksum.js](micro/104-byte-array-checksum.js) | Adler-32 over a Uint8Array, written through an Int32Array of words (zlib's style). |
| [105-string-charcodeat-scan.js](micro/105-string-charcodeat-scan.js) | A lexer's loop: classify each character of a string by its code. |
| [106-string-switch.js](micro/106-string-switch.js) | `switch` over string tokens (an interpreter's opcode names). |
| [107-typeof-dispatch.js](micro/107-typeof-dispatch.js) | typeof tests on an array of mixed values. |
| [108-instanceof-hierarchy.js](micro/108-instanceof-hierarchy.js) | instanceof against classes of a three-level hierarchy. |
| [109-closure-counter.js](micro/109-closure-counter.js) | A closure reading and writing a captured variable. |
| [110-callback-higher-order.js](micro/110-callback-higher-order.js) | A hand-written reduce taking a function argument, one callback per site. |
| [111-arguments-variadic.js](micro/111-arguments-variadic.js) | A variadic function reading `arguments.length` and `arguments[i]`. |
| [112-object-literal-vectors.js](micro/112-object-literal-vectors.js) | Vector math on short-lived object literals `{x, y, z}` (Raytrace's style). |
| [113-array-literal-pairs.js](micro/113-array-literal-pairs.js) | Small array literals as tuples: returned as `[quotient, remainder]`, then read. |
| [114-matrix-int-2d.js](micro/114-matrix-int-2d.js) | Integer matrix multiply over arrays of arrays. |
| [115-double-array-dot.js](micro/115-double-array-dot.js) | Dot products and axpy over plain arrays holding doubles. |
| [116-keyed-dictionary.js](micro/116-keyed-dictionary.js) | An object used as a dictionary: computed string keys stored, looked up, enumerated. |
| [117-sieve-holey-array.js](micro/117-sieve-holey-array.js) | Sieve of Eratosthenes on an array grown by indexed stores of booleans. |
| [118-accessor-properties.js](micro/118-accessor-properties.js) | A getter and a setter on a prototype (`Object.defineProperty`), used in a hot loop. |
| [119-binary-trees.js](micro/119-binary-trees.js) | Recursively allocated binary trees, walked recursively. |
| [120-linked-list-walk.js](micro/120-linked-list-walk.js) | Walking a singly linked list until null, a field read per node. |
| [121-task-state-flags.js](micro/121-task-state-flags.js) | Tasks with bit-flag states, a scheduler picking runnable ones (Richards' style). |
| [122-insertion-sort.js](micro/122-insertion-sort.js) | In-place insertion sort of an int array, re-shuffled each round. |
| [123-string-equality.js](micro/123-string-equality.js) | `===` between string literals and strings built at run time. |
| [124-mixed-int-double.js](micro/124-mixed-int-double.js) | Arithmetic on values that are ints or doubles at the same site. |
| [125-megamorphic-get.js](micro/125-megamorphic-get.js) | One load site seeing eight shapes, the field at different offsets. |
| [126-call-apply-forward.js](micro/126-call-apply-forward.js) | Forwarding through `Function.prototype.call` and `apply` to a method. |
| [127-try-catch-hot.js](micro/127-try-catch-hot.js) | A hot function with a try/catch whose catch rarely runs. |
| [128-string-build-concat.js](micro/128-string-build-concat.js) | Building strings with `+=` in a loop, then reading length and a character. |
| [129-int-div-mod.js](micro/129-int-div-mod.js) | Integer division by `(a / b) \| 0` and `%` with variable and constant divisors. |
| [130-loop-invariant-loads.js](micro/130-loop-invariant-loads.js) | A method loop re-reading `this.items`, its length and `this.scale` each iteration. |
| [131-bounds-checked-sum.js](micro/131-bounds-checked-sum.js) | The canonical `for (i = 0; i < a.length; i++) s += a[i]`. |
| [132-double-truncation.js](micro/132-double-truncation.js) | Doubles truncated with `\| 0`, `>> 0` and Math.floor, used as array indices. |
| [133-temporary-object.js](micro/133-temporary-object.js) | A temporary object built and consumed inside one function: it never escapes. |
| [134-strength-reduction.js](micro/134-strength-reduction.js) | Multiplies, divides and modulos by constant powers of two, and constant folding. |
| [135-compare-chain.js](micro/135-compare-chain.js) | An if/else-if chain over int ranges. |

Groups:

- **0x, scalars**: loops, int and double arithmetic, `||` defaults,
  `switch`, string concatenation, loose equality.
- **1x, calls**: global functions, "static" methods on constructors,
  missing arguments, parent constructors and methods through `.call`, inner
  functions, constructors run for their side effects, parent constructors
  calling overridden methods, checks that throw.
- **2x–3x, objects**: constructor-built shapes, monomorphic and polymorphic
  loads, stores and calls, prototype chains, field orders that differ across
  subclasses, `Object.prototype` extension, constant properties of global
  objects, identity singletons, null fields, load chains, implementations
  at mixed prototype depths, empty polymorphic methods.
- **4x, arrays**: `push`, `pop`, `length`, indexed loads, in-place
  compaction, the collection wrapper, scans over elements of several shapes.
- **5x, globals**: a global object used from hot methods, a reassigned
  global, numbers stored into an external object (the barrier fast path),
  top-level loops, reentrant calls through a reassigned global.
- **6x–7x, escape and flattening**: local worklist wrappers (A), wrappers
  passed to helpers, returned and aliased collections, owned children (B),
  dead fields, graphs built by constructors, arguments to external methods,
  one-time externalization of a graph, a free region per call, allocation
  while the previous instance is live, per-call temporaries.
- **8x, kernels**: multi-class workloads: plan execution (copy,
  scale), rank propagation over a worklist, collecting and re-adding by
  rank, building and tearing down a graph, plan extraction, phases with
  different feedback, and a whole DeltaBlue-style solver.
- **9x, calls into compiled code**: recursion (self, mutual, tail),
  constructors called in a loop, a native method next to one.
- **10x–13x, other engines' patterns**: kernels in the style of the rest
  of Octane and of common JS, mostly to compare toi's code with Maglev's
  (`scripts/vs-maglev.sh`): bit operations, doubles past int32, `Math`,
  typed arrays, string scanning and `switch`, `typeof`, `instanceof`,
  closures and callbacks, `arguments`, object and array literals, arrays
  of arrays and of doubles, dictionaries, holey arrays, accessors,
  recursive trees, linked lists, bit-flag state machines, sorting, string
  equality and building, mixed int/double values, megamorphic loads,
  `call`/`apply`, `try`/`catch`, integer division, loop-invariant loads,
  bounds checks, truncation, non-escaping temporaries, strength reduction,
  compare chains. Each hot function has a name of its own.

## `gc/`: the collector's open problems

Each file reproduces something the collector still does badly (see the
end of step 6 in [docs/gc.md](../docs/gc.md)), with the same
conventions as `micro/` plus a `// gc:` line on what it shows. A
seeded Park-Miller generator (exact in doubles) picks the random
positions. `scripts/gc-micro.sh` runs them under `TOI_GC_STATS` and
prints wall time, peak RSS, the live size full collections found,
collecting's share of the time, and the longest minor and major pauses.

| file | what it shows |
|---|---|
| [survivors.js](gc/survivors.js) | Nearly the whole nursery survives each minor collection, then dies old. |
| [cards.js](gc/cards.js) | Young objects stored into random old records: minor pauses are card scanning. |
| [fragmentation.js](gc/fragmentation.js) | Records replaced at random: a non-moving heap peaks at several times live. |
| [splay.js](gc/splay.js) | Octane's Splay in small: all of the above, and a long final pause. |
