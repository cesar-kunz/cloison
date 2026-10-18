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

use crate::builtins::{caught, is_class, link_constructor, make_error, run_source};
use crate::collections::iterate;
use crate::interp::{call_function, to_string_units};
use crate::jit::Ty;
use crate::object::{Class, DONT_ENUM, Object, as_object, object_value};
use crate::props::{is_callable, is_object};
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
    /// … and an async function's `await`'s.
    AsyncFulfilled,
    AsyncRejected,
}

/// Promise states, in the state cell (and the tag of a reaction job).
const PENDING: i32 = 0;
const FULFILLED: i32 = 1;
const REJECTED: i32 = 2;
/// The tag of a `PromiseResolveThenableJob`.
const THENABLE: i32 = 3;

/// The hidden array's cells: the state, the result, then the reactions.
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
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = Ty::Any;
    let proto = vm.new_object(op);
    let c = vm.define_native(g, "Promise", promise_ctor, 1, any, true);
    link_constructor(vm, c, proto);
    let jobs = as_object(vm.new_array());

    // `AggregateError`, as the error kinds are made in `builtins`.
    let ep = object_value(vm.error_prototypes[0]);
    let aep = vm.new_object(ep);
    let ac = vm.define_native(g, "AggregateError", aggregate_error_ctor, 2, any, true);
    link_constructor(vm, ac, aep);
    let (a_name, a_message) = (vm.intern("name"), vm.intern("message"));
    let name = string(vm, "AggregateError");
    vm.add_property_with(aep, a_name, name, DONT_ENUM);
    let empty = string(vm, "");
    vm.add_property_with(aep, a_message, empty, DONT_ENUM);

    vm.promise_roots = vec![proto, c, jobs, aep, std::ptr::null_mut()];
    let templates: [(NativeFn, u32); 13] = [
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
        (async_fulfilled, 1),
        (async_rejected, 1),
    ];
    for (f, n) in templates {
        let t = vm.native_function("", f, n, any, false);
        vm.promise_roots.push(t);
    }
    vm.define_natives(
        c,
        &[
            ("resolve", promise_resolve as NativeFn, 1),
            ("reject", promise_reject, 1),
            ("all", promise_all, 1),
            ("race", promise_race, 1),
            ("allSettled", promise_all_settled, 1),
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

// ---- Async functions ----

/// A new pending promise and its resolve and reject functions.
pub(crate) fn new_with_resolvers(vm: &mut Vm) -> (Value, Value, Value) {
    let p = new_promise(vm);
    let (resolve, reject) = resolving_functions(vm, p);
    (p, resolve, reject)
}

/// An `await`'s reaction for coroutine `g`: going on with the value
/// (`rejected`: with the reason thrown).
pub(crate) fn async_closure(vm: &mut Vm, rejected: bool, g: Value) -> Value {
    closure(vm, if rejected { Root::AsyncRejected } else { Root::AsyncFulfilled }, &[g])
}

/// `await v`: PromiseResolve(%Promise%, v), then the reactions.
pub(crate) fn await_then(vm: &mut Vm, v: Value, on_fulfilled: Value, on_rejected: Value) -> Result<()> {
    let c = object_value(root(vm, Root::Constructor));
    let p = promise_resolve_with(vm, c, v)?;
    let cap = new_capability(vm, c)?;
    perform_then(vm, as_object(p), on_fulfilled, on_rejected, &cap)
}

fn async_fulfilled(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let g = slot(vm.callee, 0);
    crate::generator::async_step(vm, g, crate::generator::NEXT, arg(a, 0))?;
    Ok(Value::UNDEFINED)
}

fn async_rejected(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let g = slot(vm.callee, 0);
    crate::generator::async_step(vm, g, crate::generator::THROW, arg(a, 0))?;
    Ok(Value::UNDEFINED)
}

// ---- Helpers ----

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn string(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

fn root(vm: &Vm, r: Root) -> *mut Object {
    vm.promise_roots[r as usize]
}

/// Whether `v` is `Promise` itself.
fn is_intrinsic(vm: &Vm, v: Value) -> bool {
    v.is_cell() && as_object(v) == root(vm, Root::Constructor)
}

fn is_constructor(v: Value) -> bool {
    v.is_cell()
        && unsafe {
            match (*as_object(v)).class {
                Class::Function => true,
                Class::Native => (*(*as_object(v)).code).constructs,
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

fn len(e: *mut Object) -> usize {
    unsafe { (*e).elems_len as usize }
}

/// Element `i` of array `e` (within its length).
fn elem(e: *mut Object, i: usize) -> Value {
    unsafe { *(*e).elems.add(i) }
}

/// Set element `i` of array `e` (within its length) to `v`.
fn put(e: *mut Object, i: usize, v: Value) {
    unsafe {
        let at = (*e).elems.add(i);
        *at = v;
        crate::heap::barrier((*e).elems.cast::<u8>().sub(8), at, v);
    }
}

/// Drop array `e`'s elements from `n` on.
fn truncate(e: *mut Object, n: usize) {
    for i in n..len(e) {
        put(e, i, Value::UNDEFINED);
    }
    unsafe { (*e).elems_len = n as u32 };
}

// ---- Closures ----

/// A native function of `template`'s code with `data` in hidden in-object
/// slots: a closure. The slots are beyond its shape's capacity (a native's
/// shape, with none), so JS never reaches them, while the collector traces
/// them by the object's own capacity (see `object`'s docs: a shape's
/// capacity may be below its object's). The native reads them through
/// `Vm::callee`.
fn closure(vm: &mut Vm, template: Root, data: &[Value]) -> Value {
    let fp = object_value(vm.function_prototype);
    let full = vm.root_shape(Class::Native, fp, data.len() as u32);
    let bare = vm.root_shape(Class::Native, fp, 0);
    let o = vm.heap.alloc_object(unsafe { &*full });
    unsafe {
        (*o).shape = bare;
        (*o).code = (*root(vm, template)).code;
        for (i, &v) in data.iter().enumerate() {
            set_slot(o, i, v);
        }
    }
    object_value(o)
}

/// Hidden slot `i` of closure `o`.
fn slot(o: *mut Object, i: usize) -> Value {
    unsafe { *Object::inline(o, i) }
}

fn set_slot(o: *mut Object, i: usize, v: Value) {
    unsafe {
        let at = Object::inline(o, i);
        *at = v;
        crate::heap::barrier(o.cast(), at, v);
    }
}

// ---- Promise objects ----

/// A new pending promise (of `Promise.prototype`).
fn new_promise(vm: &mut Vm) -> Value {
    let cells = vm.new_array();
    array_push(vm, cells, &[Value::from_i32(PENDING), Value::UNDEFINED]).expect("an array");
    let proto = object_value(root(vm, Root::Prototype));
    let o = vm.new_object_with(Class::Promise, proto, 0);
    unsafe {
        (*o).code = cells.to_bits() as *const Code;
        crate::heap::barrier(o.cast(), &raw const (*o).code, cells);
    }
    object_value(o)
}

/// Promise `p`'s hidden array.
fn cells(p: *mut Object) -> *mut Object {
    unsafe { as_object(Value::from_bits((*p).code as u64)) }
}

fn this_promise(this: Value, what: &str) -> Result<*mut Object> {
    if is_class(this, Class::Promise) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error(format!("{what} called on incompatible receiver {}", crate::vm::display(this))))
    }
}

/// FulfillPromise or RejectPromise: pending `p` settles with `value`, and
/// its reactions' jobs queue (the ones of the new state's kind run).
fn settle(vm: &mut Vm, p: *mut Object, state: i32, value: Value) -> Result<()> {
    let e = cells(p);
    if elem(e, STATE).as_i32_unchecked() != PENDING {
        return Ok(());
    }
    put(e, STATE, Value::from_i32(state));
    put(e, RESULT, value);
    let mut i = REACTIONS;
    while i + REACTION <= len(e) {
        let (promise, resolve, reject) = (elem(e, i), elem(e, i + 1), elem(e, i + 2));
        let handler = elem(e, i + if state == FULFILLED { 3 } else { 4 });
        enqueue(vm, [Value::from_i32(state), promise, resolve, reject, handler, value])?;
        i += REACTION;
    }
    truncate(e, REACTIONS);
    Ok(())
}

/// The resolve function's work (§27.2.1.3.2 from step 7): `p` fulfills
/// with `resolution` unless that is a thenable, which a job will follow
/// (or `p` itself, a TypeError).
fn resolve_promise(vm: &mut Vm, p: *mut Object, resolution: Value) -> Result<()> {
    if resolution.is_cell() && as_object(resolution) == p {
        let e = type_error(vm, "Chaining cycle detected for promise #<Promise>");
        return settle(vm, p, REJECTED, e);
    }
    if !is_object(resolution) {
        return settle(vm, p, FULFILLED, resolution);
    }
    let a_then = vm.intern("then");
    let then = match vm.get(resolution, a_then) {
        Ok(t) => t,
        Err(e) => {
            let v = thrown(vm, e)?;
            return settle(vm, p, REJECTED, v);
        }
    };
    if !is_callable(then) {
        return settle(vm, p, FULFILLED, resolution);
    }
    enqueue(vm, [Value::from_i32(THENABLE), object_value(p), resolution, then, Value::UNDEFINED, Value::UNDEFINED])
}

/// CreateResolvingFunctions: a resolve and a reject function for
/// `promise`, the first called of them disabling both. Slots: the
/// promise, the partner, whether one was called.
fn resolving_functions(vm: &mut Vm, promise: Value) -> (Value, Value) {
    let resolve = closure(vm, Root::ResolveFn, &[promise, Value::UNDEFINED, Value::FALSE]);
    let reject = closure(vm, Root::RejectFn, &[promise, resolve, Value::FALSE]);
    set_slot(as_object(resolve), 1, reject);
    (resolve, reject)
}

/// The resolving function now called, if neither of its pair was called
/// before (both are marked called).
fn take_pair(vm: &mut Vm) -> Option<*mut Object> {
    let me = vm.callee;
    if slot(me, 2) == Value::TRUE {
        return None;
    }
    set_slot(me, 2, Value::TRUE);
    set_slot(as_object(slot(me, 1)), 2, Value::TRUE);
    Some(me)
}

fn resolve_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if let Some(me) = take_pair(vm) {
        let p = as_object(slot(me, 0));
        resolve_promise(vm, p, arg(a, 0))?;
    }
    Ok(Value::UNDEFINED)
}

fn reject_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if let Some(me) = take_pair(vm) {
        let p = as_object(slot(me, 0));
        settle(vm, p, REJECTED, arg(a, 0))?;
    }
    Ok(Value::UNDEFINED)
}

// ---- Capabilities ----

/// A PromiseCapability record. For `Promise` itself, `resolve` and
/// `reject` are made only when JS must see them ([`materialize`]):
/// `undefined` before, and the promise is resolved directly.
#[derive(Clone, Copy)]
struct Cap {
    promise: Value,
    resolve: Value,
    reject: Value,
}

/// NewPromiseCapability(C): `new C(executor)`, the executor keeping the
/// resolving functions it is given.
fn new_capability(vm: &mut Vm, c: Value) -> Result<Cap> {
    if is_intrinsic(vm, c) {
        let promise = new_promise(vm);
        return Ok(Cap { promise, resolve: Value::UNDEFINED, reject: Value::UNDEFINED });
    }
    if !is_constructor(c) {
        return Err(Error::type_error(format!("{} is not a constructor", crate::vm::display(c))));
    }
    let executor = closure(vm, Root::Executor, &[Value::UNDEFINED, Value::UNDEFINED]);
    let promise = construct(vm, c, executor)?;
    let eo = as_object(executor);
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
        let h = run_source(vm, "(function (C, executor) { return new C(executor); })")?;
        vm.promise_roots[k] = as_object(h);
    }
    let h = object_value(vm.promise_roots[k]);
    call_function(vm, h, Value::UNDEFINED, &[c, executor])
}

/// The capability's executor: keeps the resolve and reject functions it
/// is called with (slots 0 and 1), once.
fn executor_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let me = vm.callee;
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
    if cap.resolve.is_undefined() {
        (cap.resolve, cap.reject) = resolving_functions(vm, cap.promise);
    }
}

/// `Call(cap.[[Resolve]], undefined, « v »)`.
fn cap_resolve(vm: &mut Vm, cap: &Cap, v: Value) -> Result<()> {
    if cap.resolve.is_undefined() {
        return resolve_promise(vm, as_object(cap.promise), v);
    }
    call_function(vm, cap.resolve, Value::UNDEFINED, &[v]).map(drop)
}

/// `Call(cap.[[Reject]], undefined, « v »)`.
fn cap_reject(vm: &mut Vm, cap: &Cap, v: Value) -> Result<()> {
    if cap.reject.is_undefined() {
        return settle(vm, as_object(cap.promise), REJECTED, v);
    }
    call_function(vm, cap.reject, Value::UNDEFINED, &[v]).map(drop)
}

// ---- Jobs ----

fn enqueue(vm: &mut Vm, job: [Value; JOB]) -> Result<()> {
    let jobs = object_value(root(vm, Root::Jobs));
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
        if let Err(e) = call_function(vm, then, thenable, &[resolve, reject]) {
            let v = thrown(vm, e)?;
            call_function(vm, reject, Value::UNDEFINED, &[v])?;
        }
        return Ok(());
    }
    let cap = Cap { promise: job[1], resolve: job[2], reject: job[3] };
    let (handler, argument) = (job[4], job[5]);
    let outcome = if handler.is_undefined() {
        if tag == FULFILLED { Ok(argument) } else { Err(argument) }
    } else {
        match call_function(vm, handler, Value::UNDEFINED, &[argument]) {
            Ok(v) => Ok(v),
            Err(e) => Err(thrown(vm, e)?),
        }
    };
    match outcome {
        Ok(v) => cap_resolve(vm, &cap, v),
        Err(v) => cap_reject(vm, &cap, v),
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
        return Err(Error::type_error(format!("Promise resolver {} is not a function", crate::vm::display(executor))));
    }
    let p = new_promise(vm);
    let (resolve, reject) = resolving_functions(vm, p);
    if let Err(e) = call_function(vm, executor, Value::UNDEFINED, &[resolve, reject]) {
        let v = thrown(vm, e)?;
        call_function(vm, reject, Value::UNDEFINED, &[v])?;
    }
    Ok(p)
}

/// SpeciesConstructor(o, %Promise%): `o.constructor`, or `Promise` if
/// that is undefined. (No symbols: `C[@@species]` is `C` for `Promise`
/// and what inherits from it, undefined for anything else.)
fn species_constructor(vm: &mut Vm, o: Value) -> Result<Value> {
    let a_constructor = vm.intern("constructor");
    let c = vm.get(o, a_constructor)?;
    let intrinsic = object_value(root(vm, Root::Constructor));
    if c.is_undefined() {
        return Ok(intrinsic);
    }
    if !is_object(c) {
        return Err(Error::type_error("The .constructor property is not an object"));
    }
    let mut k = as_object(c);
    loop {
        if k == root(vm, Root::Constructor) {
            return Ok(c);
        }
        let proto = unsafe { (*(*k).shape).proto };
        if !proto.is_cell() {
            return Ok(intrinsic);
        }
        k = as_object(proto);
    }
}

// ---- The prototype ----

/// PerformPromiseThen: a reaction with the handlers (undefined if not
/// callable) and `cap`, or its job at once if `p` is settled.
fn perform_then(vm: &mut Vm, p: *mut Object, on_fulfilled: Value, on_rejected: Value, cap: &Cap) -> Result<()> {
    let on_fulfilled = if is_callable(on_fulfilled) { on_fulfilled } else { Value::UNDEFINED };
    let on_rejected = if is_callable(on_rejected) { on_rejected } else { Value::UNDEFINED };
    let e = cells(p);
    match elem(e, STATE).as_i32_unchecked() {
        PENDING => {
            array_push(vm, object_value(e), &[cap.promise, cap.resolve, cap.reject, on_fulfilled, on_rejected])?;
        }
        state => {
            let handler = if state == FULFILLED { on_fulfilled } else { on_rejected };
            let result = elem(e, RESULT);
            enqueue(vm, [Value::from_i32(state), cap.promise, cap.resolve, cap.reject, handler, result])?;
        }
    }
    Ok(())
}

/// `promise.then(onFulfilled, onRejected)`: a promise (of the receiver's
/// constructor) for the handler's result.
fn promise_then(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let p = this_promise(this, "Promise.prototype.then")?;
    let c = species_constructor(vm, this)?;
    let cap = new_capability(vm, c)?;
    perform_then(vm, p, arg(a, 0), arg(a, 1), &cap)?;
    Ok(cap.promise)
}

/// `promise.catch(onRejected)`: `promise.then(undefined, onRejected)`.
fn promise_catch(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let a_then = vm.intern("then");
    let then = vm.get(this, a_then)?;
    call_function(vm, then, this, &[Value::UNDEFINED, arg(a, 0)])
}

/// `promise.finally(onFinally)`: `promise.then(f, g)` where `f` and `g`
/// call `onFinally`, then pass the value or reason on (through a promise
/// for what `onFinally` returns, so that a rejection or a wait of its
/// counts).
fn promise_finally(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !is_object(this) {
        return Err(Error::type_error("Promise.prototype.finally called on a non-object"));
    }
    let c = species_constructor(vm, this)?;
    let on_finally = arg(a, 0);
    let (then_fn, catch_fn) = if is_callable(on_finally) {
        (closure(vm, Root::ThenFinally, &[on_finally, c]), closure(vm, Root::CatchFinally, &[on_finally, c]))
    } else {
        (on_finally, on_finally)
    };
    let a_then = vm.intern("then");
    let then = vm.get(this, a_then)?;
    call_function(vm, then, this, &[then_fn, catch_fn])
}

/// `finally`'s fulfillment handler: `onFinally()`, then the value again
/// once a promise for the result settles. Slots: `onFinally`, `C`.
fn then_finally(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let me = vm.callee;
    let (on_finally, c) = (slot(me, 0), slot(me, 1));
    let result = call_function(vm, on_finally, Value::UNDEFINED, &[])?;
    let p = promise_resolve_with(vm, c, result)?;
    let thunk = closure(vm, Root::ValueThunk, &[arg(a, 0)]);
    let a_then = vm.intern("then");
    let then = vm.get(p, a_then)?;
    call_function(vm, then, p, &[thunk])
}

/// `finally`'s rejection handler: as [`then_finally`], the reason thrown
/// again.
fn catch_finally(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let me = vm.callee;
    let (on_finally, c) = (slot(me, 0), slot(me, 1));
    let result = call_function(vm, on_finally, Value::UNDEFINED, &[])?;
    let p = promise_resolve_with(vm, c, result)?;
    let thrower = closure(vm, Root::Thrower, &[arg(a, 0)]);
    let a_then = vm.intern("then");
    let then = vm.get(p, a_then)?;
    call_function(vm, then, p, &[thrower])
}

fn value_thunk(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(slot(vm.callee, 0))
}

fn thrower(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(Error::thrown(slot(vm.callee, 0)))
}

// ---- The statics ----

/// PromiseResolve(C, x): `x` itself if it is a promise made by `C`, else
/// a new promise of `C` resolved with it.
fn promise_resolve_with(vm: &mut Vm, c: Value, x: Value) -> Result<Value> {
    if is_class(x, Class::Promise) {
        let a_constructor = vm.intern("constructor");
        if vm.get(x, a_constructor)? == c {
            return Ok(x);
        }
    }
    let cap = new_capability(vm, c)?;
    cap_resolve(vm, &cap, x)?;
    Ok(cap.promise)
}

fn this_constructor(this: Value, what: &str) -> Result<Value> {
    if is_object(this) { Ok(this) } else { Err(Error::type_error(format!("{what} called on non-object"))) }
}

/// `Promise.resolve(x)`.
fn promise_resolve(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.resolve")?;
    promise_resolve_with(vm, c, arg(a, 0))
}

/// `Promise.reject(r)`.
fn promise_reject(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.reject")?;
    let cap = new_capability(vm, c)?;
    cap_reject(vm, &cap, arg(a, 0))?;
    Ok(cap.promise)
}

/// `Promise.withResolvers()`: `{ promise, resolve, reject }`.
fn promise_with_resolvers(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.withResolvers")?;
    let mut cap = new_capability(vm, c)?;
    materialize(vm, &mut cap);
    let op = object_value(vm.object_prototype);
    let o = vm.new_object(op);
    let names = [vm.intern("promise"), vm.intern("resolve"), vm.intern("reject")];
    vm.add_properties(o, &names, &[cap.promise, cap.resolve, cap.reject], 0);
    Ok(object_value(o))
}

/// `Promise.try(f, …args)`: a promise for `f(…args)`, rejected if it throws.
fn promise_try(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let c = this_constructor(this, "Promise.try")?;
    let cap = new_capability(vm, c)?;
    match call_function(vm, arg(a, 0), Value::UNDEFINED, a.get(1..).unwrap_or(&[])) {
        Ok(v) => cap_resolve(vm, &cap, v)?,
        Err(e) => {
            let v = thrown(vm, e)?;
            cap_reject(vm, &cap, v)?;
        }
    }
    Ok(cap.promise)
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
    let mut cap = new_capability(vm, this)?;
    materialize(vm, &mut cap);
    if let Err(e) = perform_combine(vm, this, &cap, arg(a, 0), kind) {
        let v = thrown(vm, e)?;
        cap_reject(vm, &cap, v)?;
    }
    Ok(cap.promise)
}

/// PerformPromiseAll and the others: each value iterated, through
/// `C.resolve`, gets a `then` whose handlers settle the capability: at
/// the first for `race`, at the last for the others, which collect.
fn perform_combine(vm: &mut Vm, c: Value, cap: &Cap, iterable: Value, kind: Combinator) -> Result<()> {
    let a_resolve = vm.intern("resolve");
    let promise_resolve = vm.get(c, a_resolve)?;
    if !is_callable(promise_resolve) {
        return Err(Error::type_error("Promise resolve is not a function"));
    }
    let a_then = vm.intern("then");
    let values = vm.new_array();
    // `[remaining]`: the elements not settled yet, plus one until the
    // iteration is done (shared by the element functions).
    let state = vm.new_array();
    array_push(vm, state, &[Value::from_i32(1)])?;
    let mut index = 0;
    iterate(vm, iterable, |vm, next| {
        let next_promise = call_function(vm, promise_resolve, c, &[next])?;
        let (on_fulfilled, on_rejected) = match kind {
            Combinator::Race => (cap.resolve, cap.reject),
            Combinator::All => {
                array_push(vm, values, &[Value::UNDEFINED])?;
                let data = [values, state, cap.promise, cap.resolve, Value::from_i32(index), Value::FALSE];
                (closure(vm, Root::AllElement, &data), cap.reject)
            }
            Combinator::AllSettled => {
                array_push(vm, values, &[Value::UNDEFINED])?;
                let data = [values, state, cap.promise, cap.resolve, Value::from_i32(index), Value::FALSE, Value::UNDEFINED];
                let f = closure(vm, Root::SettledFulfill, &data);
                let mut data = data;
                data[6] = f;
                let r = closure(vm, Root::SettledReject, &data);
                set_slot(as_object(f), 6, r);
                (f, r)
            }
            Combinator::Any => {
                array_push(vm, values, &[Value::UNDEFINED])?;
                let data = [values, state, cap.promise, cap.reject, Value::from_i32(index), Value::FALSE];
                (cap.resolve, closure(vm, Root::AnyElement, &data))
            }
        };
        if kind != Combinator::Race {
            let s = as_object(state);
            put(s, 0, Value::from_i32(elem(s, 0).as_i32_unchecked() + 1));
        }
        let then = vm.get(next_promise, a_then)?;
        call_function(vm, then, next_promise, &[on_fulfilled, on_rejected])?;
        index += 1;
        Ok(())
    })?;
    if kind != Combinator::Race {
        let s = as_object(state);
        let remaining = elem(s, 0).as_i32_unchecked() - 1;
        put(s, 0, Value::from_i32(remaining));
        if remaining == 0 {
            if kind == Combinator::Any {
                let e = aggregate_error(vm, values);
                cap_reject(vm, cap, e)?;
            } else {
                cap_resolve(vm, cap, values)?;
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

/// An element function's slots: the values array, the `[remaining]`
/// array, the capability's promise and the function to settle it with,
/// the element's index, whether it was called (and for `allSettled`, the
/// partner sharing that).
const VALUES: usize = 0;
const REMAINING: usize = 1;
const SETTLE: usize = 3;
const INDEX: usize = 4;
const CALLED: usize = 5;
const PARTNER: usize = 6;

/// Store `v` as the element function `me`'s value, and settle the
/// capability if it was the last: with the values, or (`any`) an
/// `AggregateError` of them.
fn element_done(vm: &mut Vm, me: *mut Object, v: Value) -> Result<Value> {
    let values = slot(me, VALUES);
    vm.set_keyed(values, slot(me, INDEX), v)?;
    let s = as_object(slot(me, REMAINING));
    let remaining = elem(s, 0).as_i32_unchecked() - 1;
    put(s, 0, Value::from_i32(remaining));
    if remaining == 0 {
        let any = unsafe { (*me).code } == unsafe { (*root(vm, Root::AnyElement)).code };
        let result = if any { aggregate_error(vm, values) } else { values };
        call_function(vm, slot(me, SETTLE), Value::UNDEFINED, &[result])?;
    }
    Ok(Value::UNDEFINED)
}

/// The element function now called, if not called before (it is marked,
/// and its partner if it has one).
fn take_element(vm: &mut Vm) -> Option<*mut Object> {
    let me = vm.callee;
    if slot(me, CALLED) == Value::TRUE {
        return None;
    }
    set_slot(me, CALLED, Value::TRUE);
    if unsafe { Object::inline_cap(me) } > PARTNER {
        set_slot(as_object(slot(me, PARTNER)), CALLED, Value::TRUE);
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
    let o = vm.new_object(op);
    let names = [vm.intern("status"), vm.intern(key)];
    let status = string(vm, status);
    vm.add_properties(o, &names, &[status, v], 0);
    object_value(o)
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
    let proto = object_value(root(vm, Root::AggregateErrorPrototype));
    let o = vm.new_object(proto);
    unsafe { (*o).flags |= crate::object::ERROR };
    if let Some(m) = message {
        let a = vm.intern("message");
        let m = vm.new_string(m);
        vm.add_property_with(o, a, m, DONT_ENUM);
    }
    let a = vm.intern("errors");
    vm.add_property_with(o, a, errors, DONT_ENUM);
    object_value(o)
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
    let errors = vm.new_array();
    iterate(vm, arg(a, 0), |vm, e| array_push(vm, errors, &[e]).map(drop))?;
    Ok(new_aggregate_error(vm, errors, message.as_deref()))
}
