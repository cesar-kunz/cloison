//! `Promise` (ECMA-262 §27.2): the constructor and its statics,
//! `Promise.prototype.then`, `catch` and `finally`, `AggregateError`
//! (§20.5.7) for `Promise.any`, and the job queue (§9.5) the reactions run
//! on. Installed by [`install`].
//!
//! A promise keeps its state in a hidden array in its `code` bits (as the
//! collections do): `[state, result, reaction…]`, each reaction the five
//! values `[promise, resolve, reject, onFulfilled, onRejected]`: the
//! capability a `then` made and its handlers. A capability made for
//! `Promise` itself has no resolving functions until something must hand
//! them to JS (`resolve` undefined, see [`Cap`]): its promise is resolved
//! directly, which is what they would do.
//!
//! The functions the spec creates with state of their own (a promise's
//! resolving functions, a capability's executor, `all`'s element
//! functions, …) are natives with hidden in-object slots: see [`closure`].
//!
//! Jobs queue in a hidden array (rooted through `Vm::promise_roots`),
//! [`JOB`] values each, and run once the script is done ([`run_jobs`],
//! from `Vm::run`), in order, the ones they queue after them.

use crate::brand::{Fresh, Handle, Rootable, Scope};
use crate::builtins::{caught, is_class, link_constructor, make_error, run_source};
use crate::collections::iterate;
use crate::interp::to_string_units;
use crate::jit::Ty;
use crate::object::{Class, DONT_ENUM, Obj, Object, as_object, object_value};
use crate::props::{is_callable, is_object};
use crate::raw::{Live, ObjectPtr};
use crate::value::Value;
use crate::vm::{Code, Error, NativeFn, Result, Vm, array_push};

/// Indices into `Vm::promise_roots`.
#[derive(Clone, Copy)]
#[repr(usize)]
enum Root {
    Prototype,
    Constructor,
    /// The job queue.
    Jobs,
    AggregateErrorPrototype,
    /// `(function (C, executor) { return new C(executor); })`, made on
    /// first use (null before): `new` of a bytecode constructor.
    Construct,
    // Templates of the closures (`closure` takes their code): a promise's
    // resolving functions, …
    ResolveFn,
    RejectFn,
    /// … a capability's executor, …
    Executor,
    /// … `all`'s, `allSettled`'s and `any`'s element functions, …
    AllElement,
    SettledFulfill,
    SettledReject,
    AnyElement,
    /// … and `finally`'s.
    ThenFinally,
    CatchFinally,
    ValueThunk,
    Thrower,
    /// … AsyncFromSyncIterator's rejection handler.
    CloseOnRejection,
    /// (Not a promise's: a direct `eval`'s binding's accessors.)
    BindingGet,
    BindingSet,
}

/// Promise states, in the state cell (and the tag of a reaction job).
const PENDING: i32 = 0;
const FULFILLED: i32 = 1;
const REJECTED: i32 = 2;
/// The tag of a `PromiseResolveThenableJob`.
const THENABLE: i32 = 3;

/// A promise's state's cells (a buffer of values, its `code`): the state,
/// the result, and its reactions' array (undefined till one is added).
/// (An array for all of them: a pending `async` call's promise took 2
/// cells and 80 bytes, now 1 and 32.)
const STATE: usize = 0;
const RESULT: usize = 1;
const REACTIONS: usize = 2;
/// A reaction's width: `[promise, resolve, reject, onFulfilled, onRejected]`.
const REACTION: usize = 5;
/// A job's width: `[tag, promise, resolve, reject, handler, argument]` for
/// a reaction job, `[THENABLE, promise, thenable, then, -, -]` for a
/// thenable job.
const JOB: usize = 6;

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global_obj();
    let op = object_value(vm.object_prototype);
    let any = Ty::Any;
    let proto = vm.new_plain(op);
    let c = vm.define_native(g, "Promise", promise_ctor, 1, any, true);
    link_constructor(vm, c, proto);
    // (One queue for every realm.)
    if vm.job_queue.is_null() {
        vm.job_queue = as_object(vm.new_array());
    }
    let jobs = vm.job_queue;

    // `AggregateError`, as the error kinds are made in `builtins`.
    let ep = object_value((vm.error_prototypes[0]).ptr());
    let aep = vm.new_plain(ep);
    let ac = vm.define_native(g, "AggregateError", aggregate_error_ctor, 2, any, true);
    link_constructor(vm, ac, aep);
    let (a_name, a_message) = (vm.intern_name("name"), vm.intern_name("message"));
    let name = string(vm, "AggregateError");
    vm.add_property_with(aep, a_name, name, DONT_ENUM);
    let empty = string(vm, "");
    vm.add_property_with(aep, a_message, empty, DONT_ENUM);

    vm.promise_roots = vec![proto.ptr(), c.ptr(), jobs, aep.ptr(), std::ptr::null_mut()];
    let templates: [(NativeFn, u32); 14] = [
        (resolve_fn, 1),
        (reject_fn, 1),
        (executor_fn, 2),
        (all_element, 1),
        (settled_fulfill, 1),
        (settled_reject, 1),
        (any_element, 1),
        (then_finally, 1),
        (catch_finally, 1),
        (value_thunk, 0),
        (thrower, 0),
        (close_on_rejection, 1),
        (crate::intrinsics::binding_get, 0),
        (crate::intrinsics::binding_set, 1),
    ];
    for (f, n) in templates {
        let t = vm.native_function("", f, n, any, false);
        vm.promise_roots.push((t).ptr());
    }
    vm.define_natives(
        c,
        &[
            ("resolve", promise_resolve as NativeFn, 1),
            ("reject", promise_reject, 1),
            ("all", promise_all, 1),
            ("race", promise_race, 1),
            ("allSettled", promise_all_settled, 1),
            ("allKeyed", promise_all_keyed, 1),
            ("allSettledKeyed", promise_all_settled_keyed, 1),
            ("any", promise_any, 1),
            ("withResolvers", promise_with_resolvers, 0),
            ("try", promise_try, 1),
        ],
    );
    vm.define_natives(
        proto,
        &[("then", promise_then as NativeFn, 2), ("catch", promise_catch, 1), ("finally", promise_finally, 1)],
    );
}

/// `Promise.prototype[@@toStringTag]` (`symbol::install` calls it).
pub(crate) fn install_symbols(vm: &mut Vm) {
    let p = root(vm, Root::Prototype);
    crate::symbol::to_string_tag(vm, p, "Promise");
}

// ---- Async functions ----

/// A new pending promise and its resolve and reject functions.
pub(crate) fn new_with_resolvers(vm: &mut Vm) -> (Value, Value, Value) {
    let p = new_promise(vm);
    let (resolve, reject) = resolving_functions(vm, p);
    (p, resolve, reject)
}

/// `await v`: PromiseResolve(%Promise%, v), then the reactions, with no
/// capability (Await's PerformPromiseThen since ES2019: no promise made
/// for the reactions' results, which nothing reads; with it, a pending
/// `async` call kept two promises).
pub(crate) fn await_then(vm: &mut Vm, v: Value, on_fulfilled: Value, on_rejected: Value) -> Result<()> {
    let c = object_value(root(vm, Root::Constructor).ptr());
    // (The handlers rooted: PromiseResolve may run a `constructor` getter.)
    let s = Scope::new(vm);
    let [on_fulfilled, on_rejected] = s.roots([on_fulfilled, on_rejected]);
    // take: given at once to the next call.
    let p = vm.fresh(|vm| promise_resolve_with(vm, c, v))?.take();
    perform_then(vm, Obj::of(p), on_fulfilled.v(), on_rejected.v(), &Cap::NONE)
}

/// `await v` of coroutine `g` (an async function's or async generator's):
/// as [`await_then`] with its reactions, which go on with it (fulfilled:
/// with the value, rejected: the reason thrown). The reactions name `g`
/// itself, not two closures over it: nothing else sees them, and `run_job`
/// tells a coroutine from a handler (no handler is a generator object: it
/// isn't callable).
pub(crate) fn await_coroutine(vm: &mut Vm, v: Value, g: Value) -> Result<()> {
    let c = object_value(root(vm, Root::Constructor).ptr());
    let s = Scope::new(vm);
    let g = s.root(g);
    // take: given at once to the next call.
    let p = vm.fresh(|vm| promise_resolve_with(vm, c, v))?.take();
    perform_reaction(vm, Obj::of(p), g.v(), g.v(), &Cap::NONE)
}

/// Resolve `p`, %Promise%'s, with `v`, as its resolve function would (an
/// async function's promise, which has none: nothing else can resolve it).
pub(crate) fn resolve_own(vm: &mut Vm, p: Value, v: Value) -> Result<()> {
    resolve_promise(vm, Obj::of(p), v)
}

/// Reject `p`, %Promise%'s, with `v` (see [`resolve_own`]).
pub(crate) fn reject_own(vm: &mut Vm, p: Value, v: Value) -> Result<()> {
    settle(vm, Obj::of(p), REJECTED, v)
}

/// AsyncFromSyncIterator's promise of a step's value `v`:
/// `PromiseResolve(%Promise%, v).then()`, settling as `v` does, a job
/// after, by an iterator result of sync record `record`'s `done`
/// ([`resolve_iter_result`]); rejected, sync iterator `close` closed
/// first (unless undefined: the iterator done, or a `return()`'s value).
/// (AsyncFromSyncIteratorContinuation, `closeOnRejection`.)
pub(crate) fn async_from_sync(vm: &mut Vm, v: Value, record: Value, close: Value) -> Result<Value> {
    let c = object_value(root(vm, Root::Constructor).ptr());
    // (`record` and `close` rooted across a `constructor` getter.)
    let s = Scope::new(vm);
    let [record, close] = s.roots([record, close]);
    let wrapper = match vm.fresh(|vm| promise_resolve_with(vm, c, v)) {
        Ok(w) => w.root(&s),
        Err(e) => {
            // (A rejected promise, the iterator closed first, what was
            // thrown rooted meanwhile.)
            let _thrown = s.root(e.1.unwrap_or(Value::UNDEFINED));
            let _ = crate::symbol::close(vm, close.v());
            return rejected(vm, e);
        }
    };
    // (No handler for a promise already fulfilled, as a plain value's.)
    let on_rejected = if close.v().is_undefined() || state(wrapper.v()).0 == FULFILLED {
        s.root(Value::UNDEFINED)
    } else {
        vm.mu().make(|vm| closure(vm, Root::CloseOnRejection, &[close.v()])).root(&s)
    };
    // (Its `onFulfilled`'s iterator result: the record for `resolve`, see
    // `cap_resolve`.)
    let promise = vm.mu().make(new_promise).root(&s);
    let cap = Cap { promise: promise.v(), resolve: record.v(), reject: Value::UNDEFINED };
    perform_then(vm, Obj::of(wrapper.v()), Value::UNDEFINED, on_rejected.v(), &cap)?;
    Ok(promise.v())
}

/// `Call(cap.[[Resolve]], undefined, « CreateIteratorResultObject(v,
/// done) »)` of %Promise%'s capability `p`, `done` sync record
/// `record`'s, as AsyncFromSyncIterator does (ES2025 27.1.6.2.2 step
/// 6.b, 27.1.6.4 steps 6-9), but `p` fulfilled with `v` itself: its
/// consumers read `done` from the record (`symbol::sync_done`), and no
/// iterator result is made while `Object.prototype` has no `then` to
/// find on it. Once one was looked up on the iterator result, `p` is
/// resolved with that result, as the spec has it (fulfilled with it, or
/// adopting it as a thenable if the `then` is callable), the record
/// marked for the consumers to read what `p` gives as an iterator result
/// (`symbol::set_sync_result`): a `then` getter may have changed it, or
/// keep it.
pub(crate) fn resolve_iter_result(vm: &mut Vm, p: Obj, v: Value, record: Value) -> Result<()> {
    let a_then = vm.a_then;
    if vm.lookup(vm.object_prototype_obj(), a_then).is_none() {
        return settle(vm, p, FULFILLED, v);
    }
    // (`p`, `r` and `record` rooted across a `then` getter.)
    let s = Scope::new(vm);
    let [p, record] = s.roots([object_value(p.ptr()), record]);
    let r = s.root(crate::symbol::iter_result(vm, v, crate::symbol::sync_done(record.v()))?);
    let then = match vm.get_atom_b(r.v(), a_then) {
        Ok(t) => t.take(),
        Err(e) => {
            let e = thrown(vm, e)?;
            return settle(vm, Obj::of(p.v()), REJECTED, e);
        }
    };
    crate::symbol::set_sync_result(record.v());
    if !is_callable(then) {
        return settle(vm, Obj::of(p.v()), FULFILLED, r.v());
    }
    enqueue(vm, [Value::from_i32(THENABLE), p.v(), r.v(), then, Value::UNDEFINED, Value::UNDEFINED])
}

/// A promise resolved as [`resolve_iter_result`] of `v` and sync record
/// `record` resolves it.
pub(crate) fn iter_result_promise(vm: &mut Vm, v: Value, record: Value) -> Result<Value> {
    let p = new_promise(vm);
    resolve_iter_result(vm, Obj::of(p), v, record)?;
    Ok(p)
}

/// A promise rejected with what `e` throws.
pub(crate) fn rejected(vm: &mut Vm, e: Error) -> Result<Value> {
    let reason = thrown(vm, e)?;
    let p = new_promise(vm);
    settle(vm, Obj::of(p), REJECTED, reason)?;
    Ok(p)
}

/// [`async_from_sync`]'s rejection handler: the iterator in slot 0
/// closed (its own errors lost), the reason thrown on.
fn close_on_rejection(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let _ = crate::symbol::close(vm, slot(vm.callee_obj(), 0));
    Err(Error::thrown(arg(a, 0)))
}

// ---- Helpers ----

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn string(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

fn root(vm: &Vm, r: Root) -> Obj {
    unsafe { Obj::from_ptr(vm.promise_roots[r as usize]) }
}

/// Whether `v` is `Promise` itself.
fn is_intrinsic(vm: &Vm, v: Value) -> bool {
    v.is_cell() && as_object(v) == root(vm, Root::Constructor).ptr()
}

fn is_constructor(v: Value) -> bool {
    v.is_cell()
        && unsafe {
            match Obj::of(v).class() {
                Class::Function => true,
                Class::Native => (*Obj::of(v).code()).constructs,
                _ => false,
            }
        }
}

/// The value a JS `catch` would get for `e`: what was thrown, or an error
/// object for the runtime's own. (An `unsupported` error stays one.)
fn thrown(vm: &mut Vm, e: Error) -> Result<Value> {
    match e.1 {
        Some(v) => Ok(v),
        None if e.0.starts_with("unsupported") => Err(e),
        None => Ok(caught(vm, &e)),
    }
}

/// A `TypeError` object with `message`.
fn type_error(vm: &mut Vm, message: &str) -> Value {
    let m: Vec<u16> = message.encode_utf16().collect();
    make_error(vm, 1, Some(&m))
}

fn len(e: Obj) -> usize {
    e.elems_len() as usize
}

/// Element `i` of array `e` (within its length).
fn elem(e: Obj, i: usize) -> Value {
    e.elem(i)
}

/// Set element `i` of array `e` (within its length) to `v`.
fn put(e: Obj, i: usize, v: Value) {
    unsafe {
        let at = e.elems().add(i);
        crate::heap::store(e.elems().cast::<u8>().sub(8), at, v);
    }
}

/// Drop array `e`'s elements from `n` on.
fn truncate(e: Obj, n: usize) {
    for i in n..len(e) {
        put(e, i, Value::UNDEFINED);
    }
    unsafe { (*e.ptr()).elems_len = n as u32 };
}

// ---- Closures ----

/// A native function of `template`'s code with `data` in hidden in-object
/// slots: a closure. The slots are beyond its shape's capacity (a native's
/// shape, with none), so JS never reaches them, while the collector traces
/// them by the object's own capacity (see `object`'s docs: a shape's
/// capacity may be below its object's). The native reads them through
/// `Vm::callee`.
fn closure(vm: &mut Vm, template: Root, data: &[Value]) -> Value {
    let t = root(vm, template);
    native_closure(vm, t, data)
}

/// A closure of native function `template`'s code (see [`closure`]),
/// which reads `data` with [`slot`].
pub(crate) fn native_closure(vm: &mut Vm, template: Obj, data: &[Value]) -> Value {
    let fp = object_value(vm.function_prototype);
    let full = vm.root_shape(Class::Native, fp, data.len() as u32);
    let bare = vm.root_shape(Class::Native, fp, 0);
    let o = vm.heap.alloc_object(full.live());
    unsafe {
        crate::object::set_shape(o, bare);
        (*o).code = template.code();
        for (i, &v) in data.iter().enumerate() {
            set_slot(Obj::from_ptr(o), i, v);
        }
    }
    object_value(o)
}

/// Hidden slot `i` of closure `o`.
/// A direct `eval`'s binding's getter (`setter`: its setter), a closure
/// of `data` (see `intrinsics::binding_get`).
pub(crate) fn binding_accessor(vm: &mut Vm, setter: bool, data: &[Value]) -> Value {
    closure(vm, if setter { Root::BindingSet } else { Root::BindingGet }, data)
}

/// Whether `v` is a direct `eval`'s binding's getter.
pub(crate) fn is_binding_get(vm: &Vm, v: Value) -> bool {
    crate::builtins::is_class(v, Class::Native)
        && unsafe { Obj::of(v).code() == (*root(vm, Root::BindingGet).ptr()).code }
}

pub(crate) fn slot(o: Obj, i: usize) -> Value {
    unsafe { *Object::inline(o.ptr(), i) }
}

pub(crate) fn set_slot(o: Obj, i: usize, v: Value) {
    unsafe {
        let at = Object::inline(o.ptr(), i);
        crate::heap::store(o.ptr().cast(), at, v);
    }
}

// ---- Promise objects ----

/// A new pending promise (of `Promise.prototype`).
pub(crate) fn new_promise(vm: &mut Vm) -> Value {
    let proto = object_value(root(vm, Root::Prototype).ptr());
    let o = vm.new_object_with(Class::Promise, proto, 0);
    let st = vm.heap.alloc_values(3);
    unsafe {
        *st.add(STATE) = Value::from_i32(PENDING);
        crate::heap::store_young(o.ptr().cast(), &raw mut (*o.ptr()).code, st as *const Code);
    }
    object_value(o.ptr())
}

/// Promise `p`'s state (pending 0, fulfilled 1, rejected 2) and result.
pub(crate) fn state(p: Value) -> (i32, Value) {
    let e = cells(Obj::of(p));
    (cell(e, STATE).as_i32_unchecked(), cell(e, RESULT))
}

/// Promise `p`'s state's cells (see [`STATE`]).
fn cells(p: Obj) -> *mut Value {
    p.code() as *mut Value
}

fn cell(e: *mut Value, i: usize) -> Value {
    unsafe { *e.add(i) }
}

fn put_cell(e: *mut Value, i: usize, v: Value) {
    unsafe {
        crate::heap::store(e.cast::<u8>().sub(8), e.add(i), v);
    }
}

fn this_promise(this: Value, what: &str) -> Result<Obj> {
    if is_class(this, Class::Promise) {
        Ok(Obj::of(this))
    } else {
        Err(Error::type_error(format!("{what} called on incompatible receiver {}", crate::vm::shown(this))))
    }
}

/// FulfillPromise or RejectPromise: pending `p` settles with `value`, and
/// its reactions' jobs queue (the ones of the new state's kind run).
fn settle(vm: &mut Vm, p: Obj, state: i32, value: Value) -> Result<()> {
    let e = cells(p);
    if cell(e, STATE).as_i32_unchecked() != PENDING {
        return Ok(());
    }
    put_cell(e, STATE, Value::from_i32(state));
    put_cell(e, RESULT, value);
    let list = cell(e, REACTIONS);
    if list.is_undefined() {
        return Ok(());
    }
    put_cell(e, REACTIONS, Value::UNDEFINED);
    let r = Obj::of(list);
    let mut i = 0;
    while i + REACTION <= len(r) {
        let (promise, resolve, reject) = (elem(r, i), elem(r, i + 1), elem(r, i + 2));
        let handler = elem(r, i + if state == FULFILLED { 3 } else { 4 });
        enqueue(vm, [Value::from_i32(state), promise, resolve, reject, handler, value])?;
        i += REACTION;
    }
    Ok(())
}

/// The resolve function's work (§27.2.1.3.2 from step 7): `p` fulfills
/// with `resolution` unless that is a thenable, which a job will follow
/// (or `p` itself, a TypeError).
fn resolve_promise(vm: &mut Vm, p: Obj, resolution: Value) -> Result<()> {
    if resolution.is_cell() && as_object(resolution) == p.ptr() {
        let e = type_error(vm, "Chaining cycle detected for promise #<Promise>");
        return settle(vm, p, REJECTED, e);
    }
    if !is_object(resolution) {
        return settle(vm, p, FULFILLED, resolution);
    }
    let a_then = vm.a_then;
    // (`p` rooted across a `then` getter; `resolution` too, used after.)
    let s = Scope::new(vm);
    let [p, resolution] = s.roots([object_value(p.ptr()), resolution]);
    let then = match vm.get_atom_b(resolution.v(), a_then) {
        Ok(t) => t.take(),
        Err(e) => {
            let v = thrown(vm, e)?;
            return settle(vm, Obj::of(p.v()), REJECTED, v);
        }
    };
    if !is_callable(then) {
        return settle(vm, Obj::of(p.v()), FULFILLED, resolution.v());
    }
    enqueue(vm, [Value::from_i32(THENABLE), p.v(), resolution.v(), then, Value::UNDEFINED, Value::UNDEFINED])
}

/// CreateResolvingFunctions: a resolve and a reject function for
/// `promise`, the first called of them disabling both. Slots: the
/// promise, the partner, whether one was called.
fn resolving_functions(vm: &mut Vm, promise: Value) -> (Value, Value) {
    let resolve = closure(vm, Root::ResolveFn, &[promise, Value::UNDEFINED, Value::FALSE]);
    let reject = closure(vm, Root::RejectFn, &[promise, resolve, Value::FALSE]);
    set_slot(Obj::of(resolve), 1, reject);
    (resolve, reject)
}

/// The resolving function now called, if neither of its pair was called
/// before (both are marked called).
fn take_pair(vm: &mut Vm) -> Option<Obj> {
    let me = vm.callee_obj();
    if slot(me, 2) == Value::TRUE {
        return None;
    }
    set_slot(me, 2, Value::TRUE);
    set_slot(Obj::of(slot(me, 1)), 2, Value::TRUE);
    Some(me)
}

fn resolve_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if let Some(me) = take_pair(vm) {
        let p = Obj::of(slot(me, 0));
        resolve_promise(vm, p, arg(a, 0))?;
    }
    Ok(Value::UNDEFINED)
}

fn reject_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if let Some(me) = take_pair(vm) {
        let p = Obj::of(slot(me, 0));
        settle(vm, p, REJECTED, arg(a, 0))?;
    }
    Ok(Value::UNDEFINED)
}

// ---- Capabilities ----

/// A PromiseCapability record. For `Promise` itself, `resolve` and
/// `reject` are made only when JS must see them ([`materialize`]):
/// `undefined` before, and the promise is resolved directly (or a sync
/// record for `resolve`, by an iterator result: see [`cap_resolve`]).
/// Such a `resolve` is no function: only [`async_from_sync`] makes one,
/// for a reaction of its own, never materialized nor called.
#[derive(Clone, Copy)]
struct Cap {
    promise: Value,
    resolve: Value,
    reject: Value,
}

/// A capability rooted in a scope (see `brand`).
struct RootedCap<'s> {
    promise: Handle<'s>,
    resolve: Handle<'s>,
    reject: Handle<'s>,
}

impl RootedCap<'_> {
    /// The capability now (its values' current addresses).
    fn cap(&self) -> Cap {
        Cap { promise: self.promise.v(), resolve: self.resolve.v(), reject: self.reject.v() }
    }
}

impl Rootable for Cap {
    type Rooted<'s> = RootedCap<'s>;
    fn root_in(self, s: &Scope) -> RootedCap<'_> {
        let [promise, resolve, reject] = s.roots([self.promise, self.resolve, self.reject]);
        RootedCap { promise, resolve, reject }
    }
}

impl Cap {
    /// None: a reaction's result goes nowhere (`await`'s).
    const NONE: Cap = Cap { promise: Value::UNDEFINED, resolve: Value::UNDEFINED, reject: Value::UNDEFINED };
}

/// NewPromiseCapability(C): `new C(executor)`, the executor keeping the
/// resolving functions it is given.
fn new_capability(vm: &mut Vm, c: Value) -> Result<Cap> {
    if is_intrinsic(vm, c) {
        let promise = new_promise(vm);
        return Ok(Cap { promise, resolve: Value::UNDEFINED, reject: Value::UNDEFINED });
    }
    if !is_constructor(c) {
        return Err(Error::type_error(format!("{} is not a constructor", crate::vm::shown(c))));
    }
    // (The executor rooted: its slots are read after the constructor ran.)
    let s = Scope::new(vm);
    let executor = vm.mu().make(|vm| closure(vm, Root::Executor, &[Value::UNDEFINED, Value::UNDEFINED])).root(&s);
    // take: checked, then returned; the checks don't collect.
    let promise = vm.fresh(|vm| construct(vm, c, executor.v()))?.take();
    let eo = Obj::of(executor.v());
    let (resolve, reject) = (slot(eo, 0), slot(eo, 1));
    if !is_callable(resolve) {
        return Err(Error::type_error("Promise resolve function is not callable"));
    }
    if !is_callable(reject) {
        return Err(Error::type_error("Promise reject function is not callable"));
    }
    Ok(Cap { promise, resolve, reject })
}

/// `new c(executor)`: a native constructs itself; a bytecode function
/// through the JS helper (the runtime has no `new` of its own).
fn construct(vm: &mut Vm, c: Value, executor: Value) -> Result<Value> {
    if is_class(c, Class::Native) {
        return vm.construct_other(c, &[executor]);
    }
    let k = Root::Construct as usize;
    if vm.promise_roots[k].is_null() {
        let src = "(function (C, executor) { return new C(executor); })";
        let s = Scope::new(vm);
        let _held = (s.root(c), s.root(executor));
        // take: stored in promise_roots at once.
        let h = vm.fresh(|vm| run_source(vm, src))?.take();
        vm.promise_roots[k] = Obj::of(h).ptr();
    }
    let h = object_value(vm.promise_roots[k]);
    vm.call_b(h, Value::UNDEFINED, &[c, executor]).map(Fresh::take)
}

/// The capability's executor: keeps the resolve and reject functions it
/// is called with (slots 0 and 1), once.
fn executor_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let me = vm.callee_obj();
    if !slot(me, 0).is_undefined() {
        return Err(Error::type_error("Promise executor has already been invoked with non-undefined arguments"));
    }
    if !slot(me, 1).is_undefined() {
        return Err(Error::type_error("Promise executor has already been invoked with non-undefined arguments"));
    }
    set_slot(me, 0, arg(a, 0));
    set_slot(me, 1, arg(a, 1));
    Ok(Value::UNDEFINED)
}

/// Give `cap` its resolving functions if it has none yet.
fn materialize(vm: &mut Vm, cap: &mut Cap) {
    debug_assert!(cap.resolve.is_undefined() || is_callable(cap.resolve), "a sync record's capability handed to JS");
    if cap.resolve.is_undefined() {
        (cap.resolve, cap.reject) = resolving_functions(vm, cap.promise);
    }
}

/// `Call(cap.[[Resolve]], undefined, « v »)`; nothing without one. (A
/// `resolve` not callable: [`async_from_sync`]'s sync record, of an
/// iterator result.)
fn cap_resolve(vm: &mut Vm, cap: &Cap, v: Value) -> Result<()> {
    if cap.promise.is_undefined() {
        return Ok(());
    }
    if cap.resolve.is_undefined() {
        return resolve_promise(vm, Obj::of(cap.promise), v);
    }
    if !is_callable(cap.resolve) {
        debug_assert!(
            is_class(cap.resolve, Class::Plain),
            "a capability's resolve neither a function nor a sync record"
        );
        return resolve_iter_result(vm, Obj::of(cap.promise), v, cap.resolve);
    }
    vm.call_b(cap.resolve, Value::UNDEFINED, &[v]).map(|r| {
        r.take();
    })
}

/// `Call(cap.[[Reject]], undefined, « v »)`; nothing without one.
fn cap_reject(vm: &mut Vm, cap: &Cap, v: Value) -> Result<()> {
    if cap.promise.is_undefined() {
        return Ok(());
    }
    if cap.reject.is_undefined() {
        return settle(vm, Obj::of(cap.promise), REJECTED, v);
    }
    vm.call_b(cap.reject, Value::UNDEFINED, &[v]).map(|r| {
        r.take();
    })
}

// ---- Jobs ----

fn enqueue(vm: &mut Vm, job: [Value; JOB]) -> Result<()> {
    let jobs = object_value(root(vm, Root::Jobs).ptr());
    array_push(vm, jobs, &job)?;
    Ok(())
}

/// Run every job queued, and the ones they queue, in order.
pub(crate) fn run_jobs(vm: &mut Vm) -> Result<()> {
    let jobs = root(vm, Root::Jobs);
    let mut i = 0;
    while i < len(jobs) {
        let mut job = [Value::UNDEFINED; JOB];
        for (k, v) in job.iter_mut().enumerate() {
            *v = elem(jobs, i + k);
            put(jobs, i + k, Value::UNDEFINED);
        }
        i += JOB;
        // A long run of jobs: the ones done make room at the front.
        if i >= 4096 * JOB {
            let n = len(jobs);
            for k in i..n {
                put(jobs, k - i, elem(jobs, k));
            }
            truncate(jobs, n - i);
            i = 0;
        }
        // (ClearKeptObjects, before each job.)
        vm.kept_alive.clear();
        run_job(vm, &job)?;
        // The async waiters notified or timed out meanwhile.
        if vm.shared.waiting() {
            crate::shared::poll(vm)?;
        }
    }
    truncate(jobs, 0);
    Ok(())
}

/// A PromiseReactionJob: the handler's result (or the argument itself
/// without one) resolves the capability, what it throws rejects it. A
/// PromiseResolveThenableJob: `thenable.then(resolve, reject)` with fresh
/// resolving functions of the promise.
fn run_job(vm: &mut Vm, job: &[Value; JOB]) -> Result<()> {
    let tag = job[0].as_i32_unchecked();
    if tag == THENABLE {
        let (promise, thenable, then) = (job[1], job[2], job[3]);
        let (resolve, reject) = resolving_functions(vm, promise);
        // (`reject` rooted: the job's values are only in Rust's copy.)
        let s = Scope::new(vm);
        let reject = s.root(reject);
        if let Err(e) = vm.call_b(then, thenable, &[resolve, reject.v()]) {
            let v = thrown(vm, e)?;
            vm.call_b(reject.v(), Value::UNDEFINED, &[v])?.take();
        }
        return Ok(());
    }
    let cap = Cap { promise: job[1], resolve: job[2], reject: job[3] };
    let (handler, argument) = (job[4], job[5]);
    // (An `await`'s coroutine: see `await_coroutine`.)
    if handler.is_cell() && Obj::of(handler).class() == Class::Generator {
        let mode = if tag == FULFILLED { crate::generator::NEXT } else { crate::generator::THROW };
        return crate::generator::async_step(vm, handler, mode, argument);
    }
    if handler.is_undefined() {
        return if tag == FULFILLED { cap_resolve(vm, &cap, argument) } else { cap_reject(vm, &cap, argument) };
    }
    // (The capability rooted across the handler: the job's values are
    // only in Rust's copy.)
    let s = Scope::new(vm);
    let cap = s.hold(cap);
    match vm.call_b(handler, Value::UNDEFINED, &[argument]) {
        Ok(v) => {
            let v = v.take();
            cap_resolve(vm, &cap.cap(), v)
        }
        Err(e) => {
            let v = thrown(vm, e)?;
            cap_reject(vm, &cap.cap(), v)
        }
    }
}

// ---- The constructor ----

/// `new Promise(executor)`: `executor(resolve, reject)`, what it throws
/// rejecting the promise.
fn promise_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Promise constructor cannot be invoked without 'new'"));
    }
    let executor = arg(a, 0);
    if !is_callable(executor) {
        return Err(Error::type_error(format!("Promise resolver {} is not a function", crate::vm::shown(executor))));
    }
    let s = Scope::new(vm);
    let p = vm.mu().make(new_promise).root(&s);
    let (resolve, reject) = resolving_functions(vm, p.v());
    let reject = s.root(reject);
    if let Err(e) = vm.call_b(executor, Value::UNDEFINED, &[resolve, reject.v()]) {
        let v = thrown(vm, e)?;
        vm.call_b(reject.v(), Value::UNDEFINED, &[v])?.take();
    }
    Ok(p.v())
}

/// SpeciesConstructor(o, %Promise%): `o.constructor[Symbol.species]`, or
/// `Promise` if either is undefined (or the species null).
fn species_constructor(vm: &mut Vm, o: Value) -> Result<Value> {
    let a_constructor = vm.intern_name("constructor");
    // (`c`, maybe a getter's result only, the receiver after a proxy's
    // handler ran: rooted.)
    let sc = Scope::new(vm);
    let c = vm.get_atom_b(o, a_constructor)?.root(&sc);
    let intrinsic = object_value(root(vm, Root::Constructor).ptr());
    if c.v().is_undefined() {
        return Ok(intrinsic);
    }
    if !is_object(c.v()) {
        return Err(Error::type_error("The .constructor property is not an object"));
    }
    let a_species = vm.a_sym_species;
    let s = vm.get_atom_b(c.v(), a_species)?;
    if s.peek().is_undefined() || s.peek().is_null() {
        return Ok(intrinsic);
    }
    if crate::intrinsics::is_constructor(s.peek()) {
        return Ok(s.peek());
    }
    Err(Error::type_error("object.constructor[Symbol.species] is not a constructor"))
}

// ---- The prototype ----

/// PerformPromiseThen: a reaction with the handlers (undefined if not
/// callable) and `cap`, or its job at once if `p` is settled.
fn perform_then(vm: &mut Vm, p: Obj, on_fulfilled: Value, on_rejected: Value, cap: &Cap) -> Result<()> {
    let on_fulfilled = if is_callable(on_fulfilled) { on_fulfilled } else { Value::UNDEFINED };
    let on_rejected = if is_callable(on_rejected) { on_rejected } else { Value::UNDEFINED };
    perform_reaction(vm, p, on_fulfilled, on_rejected, cap)
}

/// [`perform_then`] of handlers already checked (or a coroutine: see
/// [`await_coroutine`]).
fn perform_reaction(vm: &mut Vm, p: Obj, on_fulfilled: Value, on_rejected: Value, cap: &Cap) -> Result<()> {
    let e = cells(p);
    match cell(e, STATE).as_i32_unchecked() {
        PENDING => {
            let mut list = cell(e, REACTIONS);
            if list.is_undefined() {
                list = vm.new_array();
                put_cell(cells(p), REACTIONS, list);
            }
            array_push(vm, list, &[cap.promise, cap.resolve, cap.reject, on_fulfilled, on_rejected])?;
        }
        state => {
            let handler = if state == FULFILLED { on_fulfilled } else { on_rejected };
            let result = cell(e, RESULT);
            enqueue(vm, [Value::from_i32(state), cap.promise, cap.resolve, cap.reject, handler, result])?;
        }
    }
    Ok(())
}

/// `promise.then(onFulfilled, onRejected)`: a promise (of the receiver's
/// constructor) for the handler's result.
fn promise_then(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let p = this_promise(this, "Promise.prototype.then")?;
    // take: given at once to new_capability; only Scope::new between.
    let c = vm.fresh(|vm| species_constructor(vm, this))?.take();
    let s = Scope::new(vm);
    let cap = vm.fresh(|vm| new_capability(vm, c))?.root(&s);
    perform_then(vm, p, arg(a, 0), arg(a, 1), &cap.cap())?;
    Ok(cap.promise.v())
}

/// `promise.catch(onRejected)`: `promise.then(undefined, onRejected)`.
fn promise_catch(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let a_then = vm.a_then;
    // take: given at once to the next call.
    let then = vm.get_atom_b(this, a_then)?.take();
    vm.call_b(then, this, &[Value::UNDEFINED, arg(a, 0)]).map(Fresh::take)
}

/// `promise.finally(onFinally)`: `promise.then(f, g)` where `f` and `g`
/// call `onFinally`, then pass the value or reason on (through a promise
/// for what `onFinally` returns, so that a rejection or a wait of its
/// counts).
fn promise_finally(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !is_object(this) {
        return Err(Error::type_error("Promise.prototype.finally called on a non-object"));
    }
    let s = Scope::new(vm);
    let c = vm.fresh(|vm| species_constructor(vm, this))?.root(&s);
    let on_finally = arg(a, 0);
    let (then_fn, catch_fn) = if is_callable(on_finally) {
        let t = vm.mu().make(|vm| closure(vm, Root::ThenFinally, &[on_finally, c.v()])).root(&s);
        (t, vm.mu().make(|vm| closure(vm, Root::CatchFinally, &[on_finally, c.v()])).root(&s))
    } else {
        (s.root(on_finally), s.root(on_finally))
    };
    let a_then = vm.a_then;
    // take: given at once to the next call.
    let then = vm.get_atom_b(this, a_then)?.take();
    vm.call_b(then, this, &[then_fn.v(), catch_fn.v()]).map(Fresh::take)
}

/// `finally`'s fulfillment handler: `onFinally()`, then the value again
/// once a promise for the result settles. Slots: `onFinally`, `C`.
fn then_finally(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let me = vm.callee_obj();
    let (on_finally, c) = (slot(me, 0), slot(me, 1));
    // take: given at once to promise_resolve_with; only Scope::new between.
    let result = vm.call_b(on_finally, Value::UNDEFINED, &[])?.take();
    let s = Scope::new(vm);
    let p = vm.fresh(|vm| promise_resolve_with(vm, c, result))?.root(&s);
    let thunk = vm.mu().make(|vm| closure(vm, Root::ValueThunk, &[arg(a, 0)])).root(&s);
    let a_then = vm.a_then;
    // take: given at once to the next call.
    let then = vm.get_atom_b(p.v(), a_then)?.take();
    vm.call_b(then, p.v(), &[thunk.v()]).map(Fresh::take)
}

/// `finally`'s rejection handler: as [`then_finally`], the reason thrown
/// again.
fn catch_finally(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let me = vm.callee_obj();
    let (on_finally, c) = (slot(me, 0), slot(me, 1));
    // take: given at once to promise_resolve_with; only Scope::new between.
    let result = vm.call_b(on_finally, Value::UNDEFINED, &[])?.take();
    let s = Scope::new(vm);
    let p = vm.fresh(|vm| promise_resolve_with(vm, c, result))?.root(&s);
    let thrower = vm.mu().make(|vm| closure(vm, Root::Thrower, &[arg(a, 0)])).root(&s);
    let a_then = vm.a_then;
    // take: given at once to the next call.
    let then = vm.get_atom_b(p.v(), a_then)?.take();
    vm.call_b(then, p.v(), &[thrower.v()]).map(Fresh::take)
}

fn value_thunk(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(slot(vm.callee_obj(), 0))
}

fn thrower(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(Error::thrown(slot(vm.callee_obj(), 0)))
}

// ---- The statics ----

/// PromiseResolve(C, x): `x` itself if it is a promise made by `C`, else
/// a new promise of `C` resolved with it.
fn promise_resolve_with(vm: &mut Vm, c: Value, x: Value) -> Result<Value> {
    // (`x` rooted across a `constructor` getter and `C`'s executor; the
    // capability across `resolve`.)
    let s = Scope::new(vm);
    let [c, x] = s.roots([c, x]);
    if is_class(x.v(), Class::Promise) {
        let a_constructor = vm.intern_name("constructor");
        if vm.get_atom_b(x.v(), a_constructor)?.take() == c.v() {
            return Ok(x.v());
        }
    }
    let cap = vm.fresh(|vm| new_capability(vm, c.v()))?.root(&s);
    cap_resolve(vm, &cap.cap(), x.v())?;
    Ok(cap.promise.v())
}

fn this_constructor(this: Value, what: &str) -> Result<Value> {
    if is_object(this) { Ok(this) } else { Err(Error::type_error(format!("{what} called on non-object"))) }
}

/// `Promise.resolve(x)`.
fn promise_resolve(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.resolve")?;
    vm.fresh(|vm| promise_resolve_with(vm, c, arg(a, 0))).map(Fresh::take)
}

/// `Promise.reject(r)`.
fn promise_reject(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.reject")?;
    let s = Scope::new(vm);
    let cap = vm.fresh(|vm| new_capability(vm, c))?.root(&s);
    cap_reject(vm, &cap.cap(), arg(a, 0))?;
    Ok(cap.promise.v())
}

/// `Promise.withResolvers()`: `{ promise, resolve, reject }`.
fn promise_with_resolvers(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.withResolvers")?;
    // (Its functions made, and the object, where nothing collects.)
    // take: only allocations follow, which don't collect.
    let mut cap = vm.fresh(|vm| new_capability(vm, c))?.take();
    materialize(vm, &mut cap);
    let op = object_value(vm.object_prototype);
    let o = vm.new_plain(op);
    let names = [vm.intern_name("promise"), vm.intern_name("resolve"), vm.intern_name("reject")];
    vm.add_properties(o, &names, &[cap.promise, cap.resolve, cap.reject], 0);
    Ok(object_value(o.ptr()))
}

/// `Promise.try(f, …args)`: a promise for `f(…args)`, rejected if it
/// throws (what it returns itself if a promise of `this`).
fn promise_try(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.try")?;
    // (The capability rooted across the call; its result across a
    // `constructor` getter.)
    let s = Scope::new(vm);
    let cap = vm.fresh(|vm| new_capability(vm, c))?.root(&s);
    match vm.call_b(arg(a, 0), Value::UNDEFINED, a.get(1..).unwrap_or(&[])) {
        Ok(v) => {
            let v = v.root(&s);
            if is_class(v.v(), Class::Promise) {
                let a_constructor = vm.intern_name("constructor");
                if vm.get_atom_b(v.v(), a_constructor)?.take() == c {
                    return Ok(v.v());
                }
            }
            cap_resolve(vm, &cap.cap(), v.v())?;
        }
        Err(e) => {
            let v = thrown(vm, e)?;
            cap_reject(vm, &cap.cap(), v)?;
        }
    }
    Ok(cap.promise.v())
}

#[derive(Clone, Copy, PartialEq)]
enum Combinator {
    All,
    Race,
    AllSettled,
    Any,
}

/// `Promise.all(iterable)` and the others: a capability of `this`, which
/// what goes wrong while iterating rejects.
fn combine(vm: &mut Vm, this: Value, a: &[Value], kind: Combinator) -> Result<Value> {
    // take: rooted below; materialize doesn't collect.
    let mut cap = vm.fresh(|vm| new_capability(vm, this))?.take();
    materialize(vm, &mut cap);
    // (The capability rooted while the iteration runs JS.)
    let s = Scope::new(vm);
    let cap = s.hold(cap);
    if let Err(e) = perform_combine(vm, this, &cap, arg(a, 0), kind) {
        let v = thrown(vm, e)?;
        cap_reject(vm, &cap.cap(), v)?;
    }
    Ok(cap.promise.v())
}

/// PerformPromiseAll and the others: each value iterated, through
/// `C.resolve`, gets a `then` whose handlers settle the capability: at
/// the first for `race`, at the last for the others, which collect.
fn perform_combine(vm: &mut Vm, c: Value, cap: &RootedCap, iterable: Value, kind: Combinator) -> Result<()> {
    let a_resolve = vm.intern_name("resolve");
    // (Rooted across the iteration, which runs JS.)
    let s = Scope::new(vm);
    let promise_resolve = vm.get_atom_b(c, a_resolve)?.root(&s);
    if !is_callable(promise_resolve.v()) {
        return Err(Error::type_error("Promise resolve is not a function"));
    }
    let a_then = vm.a_then;
    let values = vm.mu().new_array().root(&s);
    // `[remaining]`: the elements not settled yet, plus one until the
    // iteration is done (shared by the element functions).
    let state = vm.mu().new_array().root(&s);
    array_push(vm, state.v(), &[Value::from_i32(1)])?;
    let mut index = 0;
    iterate(vm, iterable, |vm, next| {
        let cap_now = cap.cap();
        let (values, state) = (values.v(), state.v());
        // (Its value's place first, as the spec has it: an array no JS
        // sees, so pushing runs none.)
        if kind != Combinator::Race {
            array_push(vm, values, &[Value::UNDEFINED])?;
        }
        // take: rooted with the handlers below; only `vm.mu()` between.
        let next_promise = vm.call_b(promise_resolve.v(), c, &[next])?.take();
        // (The handlers made where nothing collects; the capability's own
        // functions its handles' values.)
        let mu = vm.mu();
        let (on_fulfilled, on_rejected) = match kind {
            Combinator::Race => (cap_now.resolve, cap_now.reject),
            Combinator::All => {
                let data = [values, state, cap_now.promise, cap_now.resolve, Value::from_i32(index), Value::FALSE];
                (mu.make(|vm| closure(vm, Root::AllElement, &data)).v(), cap_now.reject)
            }
            Combinator::AllSettled => {
                let data = [
                    values,
                    state,
                    cap_now.promise,
                    cap_now.resolve,
                    Value::from_i32(index),
                    Value::FALSE,
                    Value::UNDEFINED,
                ];
                let f = mu.make(|vm| closure(vm, Root::SettledFulfill, &data)).v();
                let mut data = data;
                data[6] = f;
                let r = mu.make(|vm| closure(vm, Root::SettledReject, &data)).v();
                set_slot(Obj::of(f), 6, r);
                (f, r)
            }
            Combinator::Any => {
                let data = [values, state, cap_now.promise, cap_now.reject, Value::from_i32(index), Value::FALSE];
                (cap_now.resolve, mu.make(|vm| closure(vm, Root::AnyElement, &data)).v())
            }
        };
        if kind != Combinator::Race {
            let s = Obj::of(state);
            put(s, 0, Value::from_i32(elem(s, 0).as_i32_unchecked() + 1));
        }
        // (The element's promise and handlers across its `then` getter.)
        let step = Scope::new(vm);
        let [next_promise, on_fulfilled, on_rejected] = step.roots([next_promise, on_fulfilled, on_rejected]);
        let then = vm.get_atom_b(next_promise.v(), a_then)?.root(&step);
        vm.call_b(then.peek(), next_promise.v(), &[on_fulfilled.v(), on_rejected.v()])?.take();
        index += 1;
        Ok(())
    })?;
    if kind != Combinator::Race {
        let st = Obj::of(state.v());
        let remaining = elem(st, 0).as_i32_unchecked() - 1;
        put(st, 0, Value::from_i32(remaining));
        if remaining == 0 {
            if kind == Combinator::Any {
                let e = aggregate_error(vm, values.v());
                cap_reject(vm, &cap.cap(), e)?;
            } else {
                cap_resolve(vm, &cap.cap(), values.v())?;
            }
        }
    }
    Ok(())
}

fn promise_all(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    combine(vm, this, a, Combinator::All)
}

fn promise_race(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    combine(vm, this, a, Combinator::Race)
}

fn promise_all_settled(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    combine(vm, this, a, Combinator::AllSettled)
}

fn promise_any(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    combine(vm, this, a, Combinator::Any)
}

/// `Promise.allKeyed(promises)` (`settled`: `allSettledKeyed`): as
/// `all` (`allSettled`) of the values of the object's own enumerable
/// properties, settled with an object of them by the same keys.
fn combine_keyed(vm: &mut Vm, this: Value, a: &[Value], settled: bool) -> Result<Value> {
    // take: rooted below; materialize doesn't collect.
    let mut cap = vm.fresh(|vm| new_capability(vm, this))?.take();
    materialize(vm, &mut cap);
    // (The capability rooted while the properties' reads run JS.)
    let s = Scope::new(vm);
    let cap = s.hold(cap);
    if let Err(e) = perform_keyed(vm, this, &cap, arg(a, 0), settled) {
        let v = thrown(vm, e)?;
        cap_reject(vm, &cap.cap(), v)?;
    }
    Ok(cap.promise.v())
}

/// PerformPromiseAllKeyed.
fn perform_keyed(vm: &mut Vm, c: Value, cap: &RootedCap, promises: Value, settled: bool) -> Result<()> {
    let a_resolve = vm.intern_name("resolve");
    // (Rooted across the properties' reads and the calls, which run JS.)
    let s = Scope::new(vm);
    let promise_resolve = vm.get_atom_b(c, a_resolve)?.root(&s);
    if !is_callable(promise_resolve.v()) {
        return Err(Error::type_error("Promise resolve is not a function"));
    }
    if !is_object(promises) {
        return Err(Error::type_error("Promise.allKeyed: the argument is not an object"));
    }
    let o = Obj::of(promises);
    let a_then = vm.a_then;
    let keys = vm.mu().new_array().root(&s);
    let values = vm.mu().new_array().root(&s);
    let state = vm.mu().new_array().root(&s);
    array_push(vm, state.v(), &[Value::from_i32(1)])?;
    let mut index = 0;
    for key in crate::proxy::own_keys(vm, o)? {
        if !vm.own_property_b(o, key)?.take().is_some_and(|d| d.enumerable == Some(true)) {
            continue;
        }
        let step = Scope::new(vm);
        let k = vm.mu().make(|vm| vm.key_value(key)).root(&step);
        // (`value` rooted across the pushes.)
        let value = vm.get_keyed_b(promises, k.v())?.root(&step);
        array_push(vm, keys.v(), &[k.v()])?;
        array_push(vm, values.v(), &[Value::UNDEFINED])?;
        let next_promise = vm.call_b(promise_resolve.v(), c, &[value.v()])?.root(&step);
        let cp = cap.cap();
        let data = [
            values.v(),
            state.v(),
            cp.promise,
            cp.resolve,
            Value::from_i32(index),
            Value::FALSE,
            Value::UNDEFINED,
            keys.v(),
        ];
        let (on_fulfilled, on_rejected) = if settled {
            let f = vm.mu().make(|vm| closure(vm, Root::SettledFulfill, &data)).root(&step);
            let mut data = data;
            data[PARTNER] = f.v();
            let r = vm.mu().make(|vm| closure(vm, Root::SettledReject, &data)).root(&step);
            set_slot(Obj::of(f.v()), PARTNER, r.v());
            (f, r)
        } else {
            (vm.mu().make(|vm| closure(vm, Root::AllElement, &data)).root(&step), cap.reject)
        };
        let st = Obj::of(state.v());
        put(st, 0, Value::from_i32(elem(st, 0).as_i32_unchecked() + 1));
        // take: given at once to the next call.
        let then = vm.get_atom_b(next_promise.v(), a_then)?.take();
        vm.call_b(then, next_promise.v(), &[on_fulfilled.v(), on_rejected.v()])?.take();
        index += 1;
    }
    let st = Obj::of(state.v());
    let remaining = elem(st, 0).as_i32_unchecked() - 1;
    put(st, 0, Value::from_i32(remaining));
    if remaining == 0 {
        // take: given at once to the next call.
        let r = vm.fresh(|vm| keyed_result(vm, keys.v(), values.v()))?.take();
        cap_resolve(vm, &cap.cap(), r)?;
    }
    Ok(())
}

/// CreateKeyedPromiseCombinatorResultObject: a null-prototype object of
/// `values` by `keys` (arrays).
fn keyed_result(vm: &mut Vm, keys: Value, values: Value) -> Result<Value> {
    // (`keys` holds the keys' primitive values, `o` is a new object with
    // no prototype: nothing here runs JS, but rooted as the types ask.)
    let s = Scope::new(vm);
    let (keys, values) = (s.root(keys), s.root(values));
    let o = vm.mu().make(|vm| object_value(vm.new_plain(Value::NULL).ptr())).root(&s);
    for i in 0..len(Obj::of(keys.v())) {
        let key = crate::intrinsics::to_property_key(vm, elem(Obj::of(keys.v()), i))?;
        vm.create_data_property(o.v(), key, elem(Obj::of(values.v()), i))?;
    }
    Ok(o.v())
}

fn promise_all_keyed(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    combine_keyed(vm, this, a, false)
}

fn promise_all_settled_keyed(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    combine_keyed(vm, this, a, true)
}

/// An element function's slots: the values array, the `[remaining]`
/// array, the capability's promise and the function to settle it with,
/// the element's index, whether it was called (and for `allSettled`, the
/// partner sharing that; a keyed combinator's, its keys).
const VALUES: usize = 0;
const REMAINING: usize = 1;
const SETTLE: usize = 3;
const INDEX: usize = 4;
const CALLED: usize = 5;
const PARTNER: usize = 6;
const KEYS: usize = 7;

/// Store `v` as the element function `me`'s value, and settle the
/// capability if it was the last: with the values, or (`any`) an
/// `AggregateError` of them.
fn element_done(vm: &mut Vm, me: Obj, v: Value) -> Result<Value> {
    let values = slot(me, VALUES);
    vm.set_keyed(values, slot(me, INDEX), v)?;
    let s = Obj::of(slot(me, REMAINING));
    let remaining = elem(s, 0).as_i32_unchecked() - 1;
    put(s, 0, Value::from_i32(remaining));
    if remaining == 0 {
        let any = me.code() == unsafe { (*root(vm, Root::AnyElement).ptr()).code };
        let result = if Object::inline_cap(me.ptr().cast_const()) > KEYS {
            vm.fresh(|vm| keyed_result(vm, slot(me, KEYS), values))?.take()
        } else if any {
            aggregate_error(vm, values)
        } else {
            values
        };
        vm.call_b(slot(me, SETTLE), Value::UNDEFINED, &[result])?.take();
    }
    Ok(Value::UNDEFINED)
}

/// The element function now called, if not called before (it is marked,
/// and its partner if it has one).
fn take_element(vm: &mut Vm) -> Option<Obj> {
    let me = vm.callee_obj();
    if slot(me, CALLED) == Value::TRUE {
        return None;
    }
    set_slot(me, CALLED, Value::TRUE);
    if Object::inline_cap(me.ptr().cast_const()) > PARTNER && slot(me, PARTNER).is_cell() {
        set_slot(Obj::of(slot(me, PARTNER)), CALLED, Value::TRUE);
    }
    Some(me)
}

/// `all`'s resolve element function.
fn all_element(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    match take_element(vm) {
        Some(me) => element_done(vm, me, arg(a, 0)),
        None => Ok(Value::UNDEFINED),
    }
}

/// `allSettled`'s resolve element function: `{ status: "fulfilled", value }`.
fn settled_fulfill(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    match take_element(vm) {
        Some(me) => {
            let r = settled_result(vm, "fulfilled", "value", arg(a, 0));
            element_done(vm, me, r)
        }
        None => Ok(Value::UNDEFINED),
    }
}

/// `allSettled`'s reject element function: `{ status: "rejected", reason }`.
fn settled_reject(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    match take_element(vm) {
        Some(me) => {
            let r = settled_result(vm, "rejected", "reason", arg(a, 0));
            element_done(vm, me, r)
        }
        None => Ok(Value::UNDEFINED),
    }
}

fn settled_result(vm: &mut Vm, status: &str, key: &str, v: Value) -> Value {
    let op = object_value(vm.object_prototype);
    let o = vm.new_plain(op);
    let names = [vm.intern_name("status"), vm.intern_name(key)];
    let status = string(vm, status);
    vm.add_properties(o, &names, &[status, v], 0);
    object_value(o.ptr())
}

/// `any`'s reject element function.
fn any_element(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    match take_element(vm) {
        Some(me) => element_done(vm, me, arg(a, 0)),
        None => Ok(Value::UNDEFINED),
    }
}

// ---- AggregateError ----

/// A new `AggregateError` of `errors` (an array), with `message` if given.
fn new_aggregate_error(vm: &mut Vm, errors: Value, message: Option<&[u16]>) -> Value {
    let proto = object_value(root(vm, Root::AggregateErrorPrototype).ptr());
    let o = vm.new_plain(proto);
    unsafe { (*o.ptr()).flags |= crate::object::ERROR };
    if let Some(m) = message {
        let a = vm.intern_name("message");
        let m = vm.new_string(m);
        vm.add_property_with(o, a, m, DONT_ENUM);
    }
    let a = vm.intern_name("errors");
    vm.add_property_with(o, a, errors, DONT_ENUM);
    object_value(o.ptr())
}

/// `any`'s rejection: every promise was rejected.
fn aggregate_error(vm: &mut Vm, errors: Value) -> Value {
    let m: Vec<u16> = "All promises were rejected".encode_utf16().collect();
    new_aggregate_error(vm, errors, Some(&m))
}

/// `new AggregateError(errors, message)` (or a call): `errors` iterated
/// into an array.
fn aggregate_error_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    vm.is_construct();
    let message = match a.get(1) {
        Some(&v) if !v.is_undefined() => {
            let u = to_string_units(v);
            if vm.conv_thrown {
                return Err(vm.take_conv_error());
            }
            Some(u)
        }
        _ => None,
    };
    // (`{ cause }` in the options read before the errors are, as
    // InstallErrorCause comes first.)
    let options = arg(a, 2);
    let a_cause = vm.intern_name("cause");
    // (`errors` and the cause rooted across the iteration.)
    let s = Scope::new(vm);
    let cause = if crate::props::is_object(options) && vm.has_key(options, crate::vm::Key::Name(a_cause))? {
        Some(vm.get_atom_b(options, a_cause)?.root(&s))
    } else {
        None
    };
    let errors = vm.mu().new_array().root(&s);
    iterate(vm, arg(a, 0), |vm, e| array_push(vm, errors.v(), &[e]).map(drop))?;
    let e = new_aggregate_error(vm, errors.v(), message.as_deref());
    if let Some(c) = cause {
        vm.add_property_with(Obj::of(e), a_cause, c.v(), crate::object::DONT_ENUM);
    }
    Ok(e)
}
