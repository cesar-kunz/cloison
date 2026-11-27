# GC hazard analysis: checking every path for unrooted cells

Status: 2026-11-26. `scripts/gc-hazards.sh` runs it locally and as CI's
"GC hazards" job. It knows the branded cells ([gc-branding.md](gc-branding.md)),
and the hazards it finds are all fixed or reviewed safe: the baseline of
unreviewed ones is empty.

This page is for someone changing the engine's Rust code: what the
analysis looks for, how it works, what it takes on trust, and what to do
when it fails.

## What it looks for

The collector frees cells nothing refers to, and runs inside any call
that may run JavaScript. Rust locals don't keep cells alive (they only
pin them, see [gc.md](gc.md)). So a *hazard* is a cell held in a Rust
local across a call that may collect, with nothing else rooting it:

```rust
let proto = crate::proxy::get_prototype_of(vm, target)?; // a trap's result
let name = vm.get_atom_b(this, a_name)?;                 // a getter: collects
vm.root_shape(Class::Native, proto, n);                  // `proto` may be freed
```

That one was real: `Function.prototype.bind` on a proxy segfaulted under
`CLOISON_GC_STRESS`, in the default mode.

Branding makes most hazards a compile error, but not all of them. A
`take()` gives a plain `Value` back, and so does a slot read, a module
helper's result or a native's `this`. CI's stress run catches a hazard
only on the paths the scripts happen to run. The analysis looks at every
path, statically.

## How it works

`tools/gc-hazards` is a rustc driver. It compiles the `cloison` library
as usual and also analyses its MIR, the compiler's simplified form of
each function. Every function, method and closure is analysed, in four
parts.

**Which functions may collect.** These are the collector's entry points
(`Vm::collect`, `collect_from`, `collect_full_now`), and any function
that reaches one of them. A call through a function pointer or a `dyn`
counts too, since it may be a native or a callback. So does a call given
a closure that may collect. Each finding names the chain of calls that
collects, such as `get_atom_b → Vm::get → … → call_getter`.

**Where each local's value comes from.** Each local is tracked forward
from its origin:
- *Fresh*: an allocation, or what a call that may collect returns (a
  `Fresh`, taken or not), numbered by call site.
- *As rooted as some parameters*: the callers' arguments are then the
  question.
- *Rooted*, in any of these ways:
  - a constant;
  - read through the `Vm`, or through a `Handle`;
  - pushed to `vm.handles`;
  - rooted in a live `Scope`: `sc.root(v)`, `sc.roots([a, b])`,
    `fresh.root(&sc)`, `gc.root(&sc)`, or a live `root_scope`;
  - given to `Vm::rooted`, while the call runs;
  - stored into rooted memory: the VM stack, or a rooted object's slots.

When a scope ends, what it rooted is fresh again.

**Which locals are live after a call.** This is computed backward: a
local is live if it, or a reference to it, is used later.

**What it reports.** There are three kinds of hazard:
- `live across`: a fresh local, live after a call that may collect.
- `passed to`: a fresh value given to a parameter that its callee holds
  across a collection.
- `captured by`: a fresh value captured by a closure that may collect.

**Summaries make it interprocedural.** Each function gets a summary:
whether it may collect, which parameters it needs rooted, what it
returns, and what it stores through each `&mut` parameter, all in terms
of its parameters. The summaries are iterated to a fixed point over all
8500 functions, about 13 rounds and 3 minutes.

**Kinds, for precision.** Some values carry a kind:
- `plain`: a new ordinary object or array, made by `vm.mu().new_plain()`
  and the like. Defining a property on one runs no JS.
- `internal`: an object no JS sees, such as an iterator's hidden state
  array. Reading and writing its elements runs no getter.

A call whose only way to collect is through an exotic receiver doesn't
collect when it is given one of these. Two more refinements:
- What holds no cell is rooted, even when read out of a tuple, such as
  the number in a `(Handle, f64)`.
- A `Result`'s `Ok` part is tracked apart from its error, the thrown
  value that `?` returns. So a thrown value's state doesn't spill into
  the success path.

## What it takes on trust

It is a heuristic, not a proof:
- A value read from an object is taken to be as rooted as that object.
  It is not checked that the object goes on holding the value. A proxy
  revoked mid-operation breaks that assumption, which is how the
  `defineProperty` bug below went unseen at first.
- A call it can't resolve is taken not to collect, unless it goes
  through a pointer.
- "Rooted" means kept alive, not kept up to date. A copy of a value
  that a handle, the VM stack or a rooted object holds counts as rooted,
  but a collection that moves the cell updates only the handle or slot,
  not the copy. That is sound in the default mode, where the native
  stack's words pin the cells they point to, so a cell a Rust local
  holds never moves. Under `CLOISON_GC_NO_NATIVE_ROOTS=drop` they
  neither root nor pin, so nothing moves there either: `drop` turns
  evacuation off, and checks liveness only. So the analysis, and
  `safe.txt`'s "held by" reasons, don't cover precise roots with a
  moving collector: that would need every copy held across a call that
  may collect re-read through its handle, a stricter rule than the one
  checked here, through the whole runtime. It isn't the design: the
  native stack pins a few KiB per collection (at most 0.034 MiB on
  Octane and the web apps, 2026-11-27), not worth that rewrite.

What it can't see is stated in `tools/gc-hazards/annotations.txt`, each
directive with its reason:

| Directive | Count | Says |
|---|---|---|
| `no-gc f` | 27 | `f` doesn't collect (realm setup defines builtins on objects no JS has seen) |
| `holds f i` | 4 | `f`'s result is held by argument `i` (a module's namespace, by its record) |
| `plain f` | 9 | `f` returns a new ordinary object or array |
| `exotic-gc f i` | 5 | `f` collects only through argument `i` being exotic (a proxy, an array's `length`) |
| `internal f` | 2 | `f` returns an object no JS sees |
| `internal-gc f i` | 3 | `f` collects only through argument `i` being one JS may see |
| `internal-holds f i` | 2 | `f`'s result from such an internal argument is held by it |
| `no-cell f` | 5 | `f` never returns a cell (the `Value` number constructors) |

An annotation naming no function fails the run, since after a rename it
would assert nothing.

## Running it

```sh
rustup component add rustc-dev llvm-tools-preview   # once, on the pinned nightly
scripts/gc-hazards.sh            # check: fails on a hazard not reviewed
scripts/gc-hazards.sh --update   # rewrite the baseline
```

The script builds the driver and runs `cargo check` of the library
through it, about 3 minutes. The driver links rustc's libraries
dynamically, so the script overrides `.cargo/config.toml`'s static host
flags with `RUSTFLAGS`. `GC_HAZARDS_TARGET` moves its target directory.

It compares what it finds with two lists, keyed by function, value and
callee (not by line, so unrelated edits don't disturb them):
- `tools/gc-hazards/safe.txt`: hazards reviewed and found safe, where no
  annotation fits, each with its reason. There are 46.
- `tools/gc-hazards/baseline.txt`: hazards found but not handled yet. It
  is empty.

A hazard in neither list fails the run. It also lists the reviewed
hazards that are no longer found, to be dropped from the lists.

For debugging:
- `GC_HAZARDS_DUMP=f,g` prints the state at every assignment and call of
  functions `f` and `g`. Give rustc's path, or the part after the impl's
  `>::`.
- `GC_HAZARDS_TRACE=part` prints the summaries and collection chains of
  the functions whose path contains `part`.

## When it fails

Each line names the place, the function, the value and the call:

```
src/builtins.rs:1364:21  builtins::function_bind  live across `proto: value::Value` → `brand::<impl vm::Vm>::get_atom_b`  get_atom_b → Vm::get → … → call_getter
```

Then:
1. **Real?** Ask whether anything but this local refers to the value
   while the call runs: JS may have dropped every other reference by
   then. If nothing does, root it in a `Scope`, and write a script that
   crashes or differs without the fix, under
   `CLOISON_GC_NO_NATIVE_ROOTS=drop CLOISON_GC_STRESS=1`.
2. **Safe for a reason the analysis could learn?** An annotation is
   better: one line covers every caller. Examples: a new function that
   returns a new plain object, or a helper that only reads an internal
   array.
3. **Safe for this call only?** Add a `safe.txt` line: the key, a tab,
   `# `, then the reason, checked against the code.
4. **Only a precision gap?** Prefer making the code plain to the
   analysis. For example, write `vm.mu().new_plain(p).root(&sc)` instead
   of wrapping it in `Mu::make`.

## What it has found

- **First run, 2026-11-23:** 1909 hazards. Refining the analysis brought
  that to 253, and all 253 were triaged:
  - about 50 were real and are rooted, each with a crash script. Among
    them, proxies' `get` and `set` held their receiver across a trap's
    getter, which segfaulted in the default mode, even without stress;
  - the rest became annotations and `safe.txt` reasons.
- **Port to the branded cells, 2026-11-25:** this taught it the branded
  forms (scopes, handles, `Fresh`, `Mu`) and the two refinements above.
  The reviewed list went from 141 entries to 46. It also found two
  use-after-frees, both live on master in the default mode, both now
  rooted with a script each:
  - `Function.prototype.bind`'s proxy prototype, the example above;
  - a proxy with no `defineProperty` trap forwarding to its target after
    being revoked, either by the trap lookup's getter or by the define's
    own `length` conversion. The target was then held only by Rust.

- **Review of `safe.txt`, 2026-11-26:** all 46 entries checked again
  against the code, each claim attacked from JavaScript (proxies from
  constructors and species, revocable proxies, `Array.prototype` index
  accessors, re-entrant async generators, `super()` returning a proxy)
  under `drop` and stress: all safe, no new bug. It found the limit on
  moving cells above.

To check that it still finds what it should, plant a hazard (a value
taken out of a `Fresh` and held across another `get_atom_b`) and run the
script: the analysis flags a plain fresh value, one used after its scope
ended, and one taken out of an `Ok`.
