# Branded cells: letting the compiler check GC safety

Status: 2026-11-25, on the `gc-handles` branch. Every module's calls that
may collect are branded, and every raw value taken out of a branded
result either is checked by the compiler or states why it is safe.
Natives take their arguments as `Args<'s>`, which only rooted values can
become, and allocate through `vm.mu()`, which gives each new cell as a
`Gc` the compiler tracks.

This page explains the idea for someone who knows Rust but not the
collector: what can go wrong, how a few types make the compiler catch
it, and where the checking stops.

## The problem

Cloison's collector frees objects nothing refers to, and moves some of
the rest. It runs at *safepoints*: inside any call that may run
JavaScript (a getter, a callback, a proxy trap, `toString`).

Rust code in the engine — a *native*, such as `Array.prototype.map` —
holds objects in ordinary local variables. The collector doesn't treat
those locals as references that keep an object alive (it only *pins*
what they point at, so it doesn't move; see [gc.md](gc.md)). So this
is a bug:

```rust
let raw = vm.get(o, "raw")?;   // a new object, referred to only by `raw`
let n = vm.length_of(o)?;      // runs a getter, which collects: `raw` is freed
vm.get_keyed(raw, 0)?;         // reads freed memory
```

Nothing in the types says that `length_of` can collect, or that `raw`
needs protecting. Before branding, only testing found these bugs: CI
runs every script with a collection at every safepoint, and with the
locals neither pinning nor keeping alive, so a missed root makes the
script crash or differ (`CLOISON_GC_STRESS=1`,
`CLOISON_GC_NO_NATIVE_ROOTS=drop`). It finds them only on the paths the
scripts happen to run.

## The idea: a result that can't outlive the next call

A call that may collect returns a **`Fresh`** value instead of a plain
`Value`. A `Fresh` keeps the `&mut Vm` borrow of the call that made it.
Rust allows one mutable borrow at a time, so while a `Fresh` is alive,
no other call on `vm` compiles:

```rust
let raw = vm.get_b(o, "raw")?;     // Fresh<'_>: still borrows `vm`
let n = vm.length_of(o)?;          // error[E0499]: `vm` borrowed twice
vm.get_keyed_b(raw.peek(), 0)?;
```

The bug above is now a compile error. The compiler doesn't know about
garbage collection; it just knows `raw` still borrows `vm`, and
`length_of` wants it too.

To fix it, root the value:

```rust
let s = Scope::new(vm);
let raw = vm.get_b(o, "raw")?.root(&s);   // a Handle: the collector sees it
let n = vm.length_of(o)?;                 // fine: `raw` no longer borrows `vm`
vm.get_keyed_b(raw.v(), 0)?;              // fine: still alive, and moved if it moved
```

This is the same trick as [gc-arena]'s `Gc<'gc>`, used differently.
gc-arena collects only *between* calls into the heap, so its brand
lasts a whole callback. Cloison collects *inside* natives, so the brand
has to end at the next call that may collect, and what must survive
that call is rooted.

## The pieces

All of them live in `src/brand.rs`.

| Type | What it is | How long it's good for |
|---|---|---|
| `Fresh<'a, T>` | the result of a call that may collect | until the next call on `vm` (the borrow checker enforces it) |
| `Scope` | a guard; values rooted in it stay alive until it's dropped | the Rust block it's in |
| `Handle<'s>` | a value rooted in a `Scope` (a slot the collector updates) | as long as its `Scope` |
| `Mu<'a>` / `Gc<'a>` | `vm.mu()`: a context in which nothing can collect, and the values made in it | while the `Mu` lives |
| `Args<'s>` | a native's arguments, rooted by the caller's frame | while the native runs |

There are three ways to use a `Fresh`:

- **`.peek()`**: read it now. The `Fresh` is still alive, so the compiler
  still forbids any call on `vm` until its last use.
- **`.root(&scope)`**: keep it. You get a `Handle`, safe across any
  number of calls.
- **`.take()`**: get the plain `Value` out. The compiler stops tracking
  it. See below.

`Fresh` is generic. A descriptor, an `Option`, a `Vec` of values or a
tuple roots to the same shape with handles inside (the `Rootable`
trait). Natives never write handles by hand.

## `take()`: the escape hatch

Often a value goes straight into the next call:

```rust
let f = vm.get_atom_b(o, a_join)?.take();
vm.call_b(f, o, &[])
```

You can't pass the `Fresh` itself: it still borrows `vm`, and
`vm.call_b(…)` borrows `vm` again. That's the borrow rule doing its job
too well. Rooting `f` is always safe, but it costs a few instructions
each time, and on a path like `forEach` or a promise reaction that's
per element or per reaction.

So `take()` means: *give me the raw value; I'm responsible for it being
safe*. Here it is safe. Nothing collects between the `get` and the
call, and inside the call `f` is the function running, so its frame
keeps it alive.

The danger is later edits. If someone adds a collecting call between
those two lines, the code still compiles. So every `take()` bound to a
name must say why it is safe, on the line above:

```rust
// take: given at once to the next call.
let f = vm.get_atom_b(o, a_join)?.take();
```

`scripts/brand-takes.py` runs in CI's lint job and fails on a taken
binding without its `// take:` line. A value used inline needs none (as
an argument, or in a test like `to_boolean(x?.take())`), and neither
does a number or a boolean made from it: no object is held.

The reasons fall into a few kinds:

- *given at once to the next call*: the callee keeps its arguments alive;
- *the call's argument, alive in its frame*: the VM keeps a call's
  arguments in the callee's frame until it returns, even if the JS code
  overwrites the parameter, and nothing collects after;
- *pushed to `vm.handles`*: rooted by hand on the next line;
- *nothing between collects*: only allocation or plain stores follow
  (allocation doesn't collect in cloison).

A `take()` is a reviewed exception, not a proof.

## Why not make every call take a `Fresh` directly?

The first plan was for calls to accept "a `Handle`, a native's argument,
or a `Fresh` given at once" and to remove `take()` altogether. Rust
can't express the last of these:

- a `Fresh` holds the `&mut Vm` borrow of its call, so passing it to
  the next call on `vm` is two mutable borrows of `vm`;
- a method that borrows `&mut self` can't hand back something that holds
  only a *shared* borrow, so the "two-phase borrow" that makes
  `v.push(v.len())` compile doesn't help either.

There's no zero-cost "valid until the next call" brand. The choices were
to root whatever is passed on (chosen), to pass values on inside
closures (`fresh.with(|vm, v| …)`, sound only if each closure makes a
single call), or to stop. Rooting where it's cheap, and a reasoned
`take()` where it isn't, kept the cost unmeasurable on Octane.

## How the modules were migrated

The method is the same for each module:

1. Switch its calls to the branded forms (`get_b`, `get_atom_b`,
   `call_b`, `to_string_b`, `to_obj_b`, …).
2. Keep each result as a `Fresh` and read it with `.peek()`. Every value
   held across a call is now a compile error naming its line.
3. For each error, root the value (if a collection can happen) or take
   it (if it is used at once).
4. Measure instructions against the previous build on microbenchmarks
   of the module's hot paths. Where a root shows up, take the value with
   a reason instead.

The borrow checker found real bugs this way, values held unrooted across
a call that runs JavaScript:

- `String.raw`'s `raw` object, across `length`'s getter (the pilot);
- `Intl.Collator`'s compare, its first argument's new string across the
  second argument's conversion;
- `Intl.Segmenter`'s `segment()`, its string across the lazy creation of
  a prototype;
- `Intl`'s locale lists, a primitive's wrapper across the elements'
  `toString` calls. It segfaulted under the stress settings, and
  `tests/cli.rs` keeps the case.

## Cost

Branding itself is free: `Fresh`, `Gc` and `Mu` are plain values, and
their methods are `#[inline(always)]`. Rooting costs one push to
`vm.handles`, popped when the `Scope` ends.

Across the whole branch:

- Octane's instruction counts are within ±0.05% (geomean), with the JIT
  and without;
- the module microbenchmarks are within +0.4%, except a few accepted
  ones up to +1.8% (`join`, `defineProperties`) from step 2.

## A native's arguments: `Args<'s>`

A native is a Rust function the VM calls with `(vm, this, args)`. Its
arguments are safe to hold across any call it makes, but only because
the caller put them somewhere the collector sees: the interpreter passes
a slice of its own frame, and `call_function` copies a Rust caller's
values to the VM stack first.

Nothing in a `&[Value]` says that. Rust code could call a native
directly with a slice of its own locals, `map_set(vm, m, &[key, v])`,
and if the native then ran JavaScript, `key` was unrooted. So natives
now take `Args<'s>`, a wrapper over the slice with the same layout and
no run-time cost, which reads like a slice (`a.len()`, `a[0]`, `for v in
a`). There are only two ways to make one:

- **the dispatch**, from a frame's registers or `call_function`'s copy,
  through an `unsafe` constructor whose safety rule is exactly that the
  values are rooted;
- **`vm.with_args(&[key, v], |vm, a| native(vm, this, a))`**, which
  copies them to the VM stack.

A Rust caller's own slice no longer compiles (`expected Args, found
&[Value; 2]`). Converting the 670 natives turned up 11 helpers that only
looked like natives, which went back to `&[Value]`, and a dozen direct
calls. Most of those now call a plain helper instead; `Map.groupBy`, for
one, reads and writes its map through `map_get_of` and `map_set_of`, not
through the `get` and `set` natives. The rest use `with_args`.

The interpreter's and the JIT's calls build `Args` from their frames
directly, so the check costs nothing at run time: Octane is unchanged.

## New cells: allocation through `Mu`

`vm.new_array()` used to return a plain `Value`: a native could hold a
new array across a call that runs JavaScript, and nothing would notice.
Allocation goes through `vm.mu()` now:

```rust
let out = vm.mu().new_array();        // Gc<'_>: borrows the VM, like a Fresh
vm.call_b(f, this, &[out.v()])?;      // fine: given at once
let n = vm.length_of(o)?;             // error[E0499] if `out` is used after
```

A `Gc` is to a new cell what `Fresh` is to a call's result. Keeping one
means rooting it, `vm.mu().new_array().root(&scope)`. Taking its raw
value into a name, with `.v()` or `.obj()`, is the same escape hatch as
`take()`, and `scripts/brand-takes.py` asks for its reason too.

clippy enforces the routing. `clippy.toml` forbids the VM's allocators
(`new_array`, `new_string`, `new_plain`, `new_object_with`, `array_of`,
`new_string_with`, `new_wrapper`, …) everywhere except `brand.rs`,
where `Mu` calls them, the interpreter, and the VM's own core
functions. Those say `#[allow(clippy::disallowed_methods)]`, one by
one. A native calling `vm.new_array()` directly is a lint error in CI.
Two allocators that look like they need this don't: `key_string` and
`atom_string` give the VM's string constants, which are always rooted.

Allocation never collects in cloison: it only *requests* a collection,
which happens at the interpreter's next safepoint. So a `Gc` held while
the code fills its object (stores, `array_push` onto it,
`create_data_property` on it, more allocation) is safe. It's the next
call that may run JavaScript that needs a root. The migration used
that rule:

- 157 allocation results bound to a name became `Gc` bindings;
- the borrow checker flagged those used across another VM call;
- where every call in between was one that never collects (a fixed
  list: the VM's fields, its store and allocation methods, filling a
  fresh object, realm setup's helpers), the value stays raw with that
  reason;
- the rest were rooted, and the hot ones measured. A root on a path
  where nothing collects went back to a reasoned raw value. That left
  34 roots: values really held across a call that may collect
  (a regexp's match pieces across `construct`, a key across
  `ToPropertyKey`, a module's source across JSON parsing).

The cost is within 1% on the microbenchmarks and nothing on Octane.

## What isn't checked yet

- **The `take()`s and raw `Gc` values**: each one reviewed and given a
  reason, checked by CI to be there.
- **Raw values that never came from a `Fresh` or a `Gc`**:
  - a value read from an object's slot;
  - what a module's own helper returns (`str_value`, `closure`, …),
    which allocates through `vm.mu()` itself but returns a plain
    `Value`.

  These are plain `Value`s and can still be held across a call.
- **The interpreter and the JIT**: their values live in the VM stack,
  which the collector scans precisely. Branding doesn't apply there.

CI's stress run stays as the safety net for all of these.

[gc-arena]: https://github.com/kyren/gc-arena
