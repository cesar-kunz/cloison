//! Explicit resource management (ECMA-262 ES2026): `Symbol.dispose` and
//! `Symbol.asyncDispose` (see `symbol`), `SuppressedError`,
//! `DisposableStack`, `AsyncDisposableStack`, the iterators' dispose
//! methods, and the dispose capabilities `using` declarations keep (see
//! the `dispose_*` intrinsics). Installed by [`install`].
//!
//! A stack (class `DisposableStack`, either kind) keeps its state in a
//! hidden array in its `code` bits: `[async, disposed, resources]`, the
//! resources a hidden array of `[value, method, kind]` triples ([`SYNC`],
//! …), disposed last to first. A `using` block's capability is such a
//! state array alone.
//!
//! Disposing synchronously is [`dispose_resources`]; asynchronously, an
//! async JS helper made on first use (as `Array.fromAsync`'s), so its
//! awaits are the language's, calling only what it is given here.

use crate::builtins::{getter, link_constructor};
use crate::interp::{call_function, to_string_units};
use crate::object::{Class, DONT_ENUM, Object, as_object, object_value};
use crate::props::{Desc, is_callable, is_object};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm};

/// How a resource is disposed of: `method` called on `value` (`SYNC`), and
/// its result awaited (`ASYNC`); a sync method in an async place (its
/// result not awaited, but a tick taken: `ASYNC_FROM_SYNC`); `adopt`'s
/// callback called with `value` (`ADOPT_SYNC`, `ADOPT_ASYNC`); nothing but
/// an await, for `await using x = null` (`NULL_ASYNC`).
pub(crate) const SYNC: i32 = 0;
pub(crate) const ASYNC: i32 = 1;
pub(crate) const ASYNC_FROM_SYNC: i32 = 2;
pub(crate) const ADOPT_SYNC: i32 = 3;
pub(crate) const ADOPT_ASYNC: i32 = 4;
pub(crate) const NULL_ASYNC: i32 = 5;

/// Indices into `Vm::dispose_roots`.
#[repr(usize)]
enum Root {
    StackPrototype,
    AsyncStackPrototype,
    SuppressedErrorPrototype,
    /// The async disposal helper (`undefined` until made).
    AsyncHelper,
}

pub(crate) fn install(vm: &mut Vm) -> Result<()> {
    let g = vm.global;
    let any = crate::jit::Ty::Any;
    let op = object_value(vm.object_prototype);
    let mark = crate::symbol::SYMBOL_MARK;

    // `SuppressedError`, an error kind of its own (as `AggregateError`).
    let ep = object_value(vm.error_prototypes[0]);
    let sep = vm.new_object(ep);
    let sc = vm.define_native(g, "SuppressedError", suppressed_error_ctor, 3, any, true);
    link_constructor(vm, sc, sep);
    // (Its prototype is `Error`, as the other kinds'.)
    let a_error = vm.intern("Error");
    let error = vm.get(object_value(g), a_error)?;
    vm.set_prototype_of(sc, error);
    let (a_name, a_message) = (vm.intern("name"), vm.intern("message"));
    let name = str_value(vm, "SuppressedError");
    vm.add_property_with(sep, a_name, name, DONT_ENUM);
    let empty = str_value(vm, "");
    vm.add_property_with(sep, a_message, empty, DONT_ENUM);

    // `DisposableStack` and `AsyncDisposableStack`.
    let a_dispose = vm.intern(&format!("{mark}Symbol.dispose"));
    let a_async_dispose = vm.intern(&format!("{mark}Symbol.asyncDispose"));
    let a_tag = vm.a_sym_to_string_tag;
    let dsp = vm.new_object(op);
    let dc = vm.define_native(g, "DisposableStack", stack_ctor::<false>, 0, any, true);
    link_constructor(vm, dc, dsp);
    vm.define_natives(
        dsp,
        &[
            ("adopt", stack_adopt::<false> as NativeFn, 2),
            ("defer", stack_defer::<false>, 1),
            ("move", stack_move::<false>, 0),
            ("use", stack_use::<false>, 1),
        ],
    );
    let dispose = vm.define_native(dsp, "dispose", stack_dispose, 0, any, false);
    vm.add_property_with(dsp, a_dispose, object_value(dispose), DONT_ENUM);
    getter(vm, dsp, "disposed", stack_disposed::<false>);
    let tag = str_value(vm, "DisposableStack");
    let _ = vm.define_own_property(dsp, Key::Name(a_tag), &Desc::data(tag, false, false, true));

    let adsp = vm.new_object(op);
    let adc = vm.define_native(g, "AsyncDisposableStack", stack_ctor::<true>, 0, any, true);
    link_constructor(vm, adc, adsp);
    vm.define_natives(
        adsp,
        &[
            ("adopt", stack_adopt::<true> as NativeFn, 2),
            ("defer", stack_defer::<true>, 1),
            ("move", stack_move::<true>, 0),
            ("use", stack_use::<true>, 1),
        ],
    );
    let dispose_async = vm.define_native(adsp, "disposeAsync", stack_dispose_async, 0, any, false);
    vm.add_property_with(adsp, a_async_dispose, object_value(dispose_async), DONT_ENUM);
    getter(vm, adsp, "disposed", stack_disposed::<true>);
    let tag = str_value(vm, "AsyncDisposableStack");
    let _ = vm.define_own_property(adsp, Key::Name(a_tag), &Desc::data(tag, false, false, true));

    // `%IteratorPrototype%[Symbol.dispose]` and
    // `%AsyncIteratorPrototype%[Symbol.asyncDispose]`.
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    let f = vm.native_function("[Symbol.dispose]", iterator_dispose, 0, any, false);
    vm.add_property_with(ip, a_dispose, object_value(f), DONT_ENUM);
    let aip = async_iterator_prototype(vm);
    let f = vm.native_function("[Symbol.asyncDispose]", async_iterator_dispose, 0, any, false);
    vm.add_property_with(aip, a_async_dispose, object_value(f), DONT_ENUM);

    vm.dispose_roots = vec![object_value(dsp), object_value(adsp), object_value(sep), Value::UNDEFINED];
    Ok(())
}

/// `%AsyncIteratorPrototype%`: `%AsyncGeneratorPrototype%`'s prototype.
fn async_iterator_prototype(vm: &mut Vm) -> *mut Object {
    let agp = vm.symbol_roots[crate::symbol::Root::AsyncGeneratorPrototype as usize];
    as_object(unsafe { (*(*agp).shape).proto })
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn str_value(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

fn root(vm: &Vm, r: Root) -> Value {
    vm.dispose_roots[r as usize]
}

// ---- Hidden arrays ----

fn elem(o: Value, i: usize) -> Value {
    unsafe { *(*as_object(o)).elems.add(i) }
}

fn put(o: Value, i: usize, v: Value) {
    let o = as_object(o);
    unsafe {
        let at = (*o).elems.add(i);
        *at = v;
        crate::heap::barrier((*o).elems.cast::<u8>().sub(8), at, v);
    }
}

fn len(o: Value) -> usize {
    unsafe { (*as_object(o)).elems_len as usize }
}

const ASYNC_SLOT: usize = 0;
const DISPOSED: usize = 1;
const RESOURCES: usize = 2;

/// A new capability's state: `[async, disposed, resources]`.
pub(crate) fn new_state(vm: &mut Vm, async_: bool) -> Result<Value> {
    let resources = vm.new_array();
    crate::array::new_array_of(vm, &[Value::from_bool(async_), Value::FALSE, resources])
}

/// Whether capability `state` has no resources.
pub(crate) fn is_empty(state: Value) -> bool {
    len(elem(state, RESOURCES)) == 0
}

// ---- Resources ----

/// GetMethod(v, `key`): undefined if absent, a TypeError if not callable.
fn get_method(vm: &mut Vm, v: Value, key: &str) -> Result<Value> {
    let a = vm.intern(&format!("{}Symbol.{key}", crate::symbol::SYMBOL_MARK));
    let m = vm.get(v, a)?;
    if m.is_undefined() || m.is_null() {
        return Ok(Value::UNDEFINED);
    }
    if !is_callable(m) {
        return Err(Error::type_error(format!("Symbol.{key} is not a function")));
    }
    Ok(m)
}

/// AddDisposableResource of value `v` to capability `state` (`async_`:
/// with hint async-dispose): an object with a dispose method, or `null`
/// or `undefined` (nothing to do, but a tick if async).
pub(crate) fn add_resource(vm: &mut Vm, state: Value, v: Value, async_: bool) -> Result<()> {
    if v.is_null() || v.is_undefined() {
        if async_ {
            push(vm, state, Value::UNDEFINED, Value::UNDEFINED, NULL_ASYNC)?;
        }
        return Ok(());
    }
    if !is_object(v) {
        return Err(Error::type_error(format!("{} is not disposable", crate::vm::display(v))));
    }
    let (method, kind) = if async_ {
        match get_method(vm, v, "asyncDispose")? {
            m if m.is_undefined() => (get_method(vm, v, "dispose")?, ASYNC_FROM_SYNC),
            m => (m, ASYNC),
        }
    } else {
        (get_method(vm, v, "dispose")?, SYNC)
    };
    if method.is_undefined() {
        return Err(Error::type_error(format!("{} is not disposable", crate::vm::display(v))));
    }
    push(vm, state, v, method, kind)
}

fn push(vm: &mut Vm, state: Value, v: Value, method: Value, kind: i32) -> Result<()> {
    crate::vm::array_push(vm, elem(state, RESOURCES), &[v, method, Value::from_i32(kind)]).map(drop)
}

/// The thrown value of `e` (a runtime error's object).
fn thrown(vm: &mut Vm, e: Error) -> Result<Value> {
    match e.1 {
        Some(v) => Ok(v),
        None if e.0.starts_with("unsupported") => Err(e),
        None => Ok(crate::builtins::caught(vm, &e)),
    }
}

/// A new `SuppressedError` of `error` suppressing `suppressed`, without a
/// message (DisposeResources').
fn suppressed_error(vm: &mut Vm, error: Value, suppressed: Value) -> Value {
    let proto = root(vm, Root::SuppressedErrorPrototype);
    let o = vm.new_object(proto);
    unsafe { (*o).flags |= crate::object::ERROR };
    let (a_error, a_suppressed) = (vm.intern("error"), vm.intern("suppressed"));
    vm.add_property_with(o, a_error, error, DONT_ENUM);
    vm.add_property_with(o, a_suppressed, suppressed, DONT_ENUM);
    object_value(o)
}

/// DisposeResources, synchronously, of capability `state`'s resources
/// after `completion`: each disposed of, last first, what one throws
/// suppressing what was thrown before (a `SuppressedError`).
pub(crate) fn dispose_resources(vm: &mut Vm, state: Value, mut completion: Result<Value>) -> Result<Value> {
    let resources = elem(state, RESOURCES);
    let empty = vm.new_array();
    put(state, RESOURCES, empty);
    let n = len(resources) / 3;
    for k in (0..n).rev() {
        let (v, method, kind) = (elem(resources, 3 * k), elem(resources, 3 * k + 1), elem(resources, 3 * k + 2));
        let r = match kind.as_i32_unchecked() {
            ADOPT_SYNC => call_function(vm, method, Value::UNDEFINED, &[v]),
            _ => call_function(vm, method, v, &[]),
        };
        if let Err(e) = r {
            let e = thrown(vm, e)?;
            completion = Err(match completion {
                Err(prev) => {
                    let prev = thrown(vm, prev)?;
                    Error::thrown(suppressed_error(vm, e, prev))
                }
                Ok(_) => Error::thrown(e),
            });
        }
    }
    completion
}

/// The async disposal helper: `(resources, hasError, error)` to a promise
/// settled once each resource is disposed of (DisposeResources with its
/// awaits), made on first use.
fn async_helper(vm: &mut Vm) -> Result<Value> {
    let h = root(vm, Root::AsyncHelper);
    if !h.is_undefined() {
        return Ok(h);
    }
    let make = crate::builtins::run_source(vm, ASYNC_DISPOSE)?;
    let h = vm.handle_scope(|vm| {
        vm.handles.push(make);
        let call = object_value(vm.native_function("call", helper_call, 2, crate::jit::Ty::Any, false));
        vm.handles.push(call);
        let suppress = object_value(vm.native_function("suppress", helper_suppress, 2, crate::jit::Ty::Any, false));
        vm.handles.push(suppress);
        call_function(vm, make, Value::UNDEFINED, &[call, suppress])
    })?;
    vm.dispose_roots[Root::AsyncHelper as usize] = h;
    Ok(h)
}

/// DisposeResources with hint async (ECMA-262 §9.5.6), its kinds as
/// [`SYNC`] and the others number them.
const ASYNC_DISPOSE: &str = r#"(function (call, suppress) {
  "use strict";
  return async function (resources, hasError, error) {
    let needsAwait = false, hasAwaited = false;
    for (let i = resources.length - 3; i >= 0; i -= 3) {
      const value = resources[i], method = resources[i + 1], kind = resources[i + 2];
      const sync = kind === 0 || kind === 3;
      if (sync && needsAwait && !hasAwaited) {
        await undefined;
        needsAwait = false;
      }
      if (kind === 5) {
        needsAwait = true;
        continue;
      }
      try {
        const r = kind === 3 || kind === 4 ? call(method, undefined, value) : call(method, value);
        if (!sync) {
          await (kind === 2 ? undefined : r);
          hasAwaited = true;
        }
      } catch (e) {
        error = hasError ? suppress(e, error) : e;
        hasError = true;
      }
    }
    if (needsAwait && !hasAwaited) await undefined;
    if (hasError) throw error;
  };
})"#;

fn helper_call(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    call_function(vm, arg(a, 0), arg(a, 1), a.get(2..).unwrap_or(&[]))
}

fn helper_suppress(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(suppressed_error(vm, arg(a, 0), arg(a, 1)))
}

/// DisposeResources with hint async of capability `state`'s resources
/// after a throw of `error` (if `has_error`): a promise.
pub(crate) fn dispose_resources_async(vm: &mut Vm, state: Value, has_error: bool, error: Value) -> Result<Value> {
    let resources = elem(state, RESOURCES);
    let empty = vm.new_array();
    put(state, RESOURCES, empty);
    let h = async_helper(vm)?;
    call_function(vm, h, Value::UNDEFINED, &[resources, Value::from_bool(has_error), error])
}

// ---- SuppressedError ----

/// `new SuppressedError(error, suppressed, message)` (or a call).
fn suppressed_error_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    vm.is_construct();
    let message = match a.get(2) {
        Some(&v) if !v.is_undefined() => {
            let u = to_string_units(v);
            if vm.conv_failed() {
                return Err(vm.take_conv_error());
            }
            Some(vm.new_string(&u))
        }
        _ => None,
    };
    let proto = root(vm, Root::SuppressedErrorPrototype);
    let o = vm.new_object(proto);
    unsafe { (*o).flags |= crate::object::ERROR };
    if let Some(m) = message {
        let a_message = vm.intern("message");
        vm.add_property_with(o, a_message, m, DONT_ENUM);
    }
    let (a_error, a_suppressed) = (vm.intern("error"), vm.intern("suppressed"));
    vm.add_property_with(o, a_error, arg(a, 0), DONT_ENUM);
    vm.add_property_with(o, a_suppressed, arg(a, 1), DONT_ENUM);
    Ok(object_value(o))
}

// ---- DisposableStack and AsyncDisposableStack ----

fn name(async_: bool) -> &'static str {
    if async_ { "AsyncDisposableStack" } else { "DisposableStack" }
}

fn new_stack(vm: &mut Vm, async_: bool) -> Result<Value> {
    let state = new_state(vm, async_)?;
    let proto = root(vm, if async_ { Root::AsyncStackPrototype } else { Root::StackPrototype });
    let o = vm.new_object_with(Class::DisposableStack, proto, 0);
    unsafe { (*o).code = state.to_bits() as *const crate::vm::Code };
    Ok(object_value(o))
}

fn stack_ctor<const ASYNC: bool>(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error(format!("Constructor {} requires 'new'", name(ASYNC))));
    }
    new_stack(vm, ASYNC)
}

/// `this`'s state, a stack of the kind (`async_`), else a TypeError.
fn state_of(this: Value, async_: bool, what: &str) -> Result<Value> {
    if this.is_cell() && unsafe { (*as_object(this)).class } == Class::DisposableStack {
        let state = Value::from_bits(unsafe { (*as_object(this)).code } as u64);
        if elem(state, ASYNC_SLOT) == Value::from_bool(async_) {
            return Ok(state);
        }
    }
    Err(Error::type_error(format!("{}.prototype.{what} called on an incompatible receiver", name(async_))))
}

/// `this`'s state, not disposed (else a ReferenceError).
fn live_state(this: Value, async_: bool, what: &str) -> Result<Value> {
    let state = state_of(this, async_, what)?;
    if elem(state, DISPOSED) == Value::TRUE {
        return Err(Error::reference_error(format!("{} already disposed", name(async_))));
    }
    Ok(state)
}

fn stack_use<const ASYNC: bool>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let state = live_state(this, ASYNC, "use")?;
    let v = arg(a, 0);
    add_resource(vm, state, v, ASYNC)?;
    Ok(v)
}

fn stack_adopt<const ASYNC: bool>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let state = live_state(this, ASYNC, "adopt")?;
    let (v, f) = (arg(a, 0), arg(a, 1));
    if !is_callable(f) {
        return Err(Error::type_error("adopt: onDispose is not a function"));
    }
    push(vm, state, v, f, if ASYNC { ADOPT_ASYNC } else { ADOPT_SYNC })?;
    Ok(v)
}

fn stack_defer<const ASYNC: bool>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let state = live_state(this, ASYNC, "defer")?;
    let f = arg(a, 0);
    if !is_callable(f) {
        return Err(Error::type_error("defer: onDispose is not a function"));
    }
    push(vm, state, Value::UNDEFINED, f, if ASYNC { self::ASYNC } else { SYNC })?;
    Ok(Value::UNDEFINED)
}

/// `move()`: a new stack (of the intrinsic prototype) with this one's
/// resources, this one disposed.
fn stack_move<const ASYNC: bool>(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = live_state(this, ASYNC, "move")?;
    let out = new_stack(vm, ASYNC)?;
    let to = Value::from_bits(unsafe { (*as_object(out)).code } as u64);
    put(to, RESOURCES, elem(state, RESOURCES));
    let empty = vm.new_array();
    put(state, RESOURCES, empty);
    put(state, DISPOSED, Value::TRUE);
    Ok(out)
}

fn stack_disposed<const ASYNC: bool>(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = state_of(this, ASYNC, "disposed")?;
    Ok(elem(state, DISPOSED))
}

fn stack_dispose(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = state_of(this, false, "dispose")?;
    if elem(state, DISPOSED) == Value::TRUE {
        return Ok(Value::UNDEFINED);
    }
    put(state, DISPOSED, Value::TRUE);
    dispose_resources(vm, state, Ok(Value::UNDEFINED)).map(|_| Value::UNDEFINED)
}

/// `disposeAsync()`: a promise, rejected for another receiver.
fn stack_dispose_async(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = match state_of(this, true, "disposeAsync") {
        Ok(s) => s,
        Err(e) => return rejected(vm, e),
    };
    if elem(state, DISPOSED) == Value::TRUE {
        let (p, resolve, _) = crate::promise::new_with_resolvers(vm);
        call_function(vm, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?;
        return Ok(p);
    }
    put(state, DISPOSED, Value::TRUE);
    dispose_resources_async(vm, state, false, Value::UNDEFINED)
}

/// A promise rejected with what `e` throws.
fn rejected(vm: &mut Vm, e: Error) -> Result<Value> {
    let v = thrown(vm, e)?;
    let (p, _, reject) = crate::promise::new_with_resolvers(vm);
    call_function(vm, reject, Value::UNDEFINED, &[v])?;
    Ok(p)
}

// ---- Iterators ----

/// `%IteratorPrototype%[Symbol.dispose]()`: `this.return()`, if it has
/// one.
fn iterator_dispose(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let a = vm.intern("return");
    let m = vm.get(this, a)?;
    if !m.is_undefined() && !m.is_null() {
        call_function(vm, m, this, &[])?;
    }
    Ok(Value::UNDEFINED)
}

/// `%AsyncIteratorPrototype%[Symbol.asyncDispose]()`: a promise of
/// `this.return()` settling (to undefined), if it has one.
fn async_iterator_dispose(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let r = (|| -> Result<Value> {
        let a = vm.intern("return");
        let m = vm.get(this, a)?;
        if m.is_undefined() || m.is_null() {
            return Ok(Value::UNDEFINED);
        }
        call_function(vm, m, this, &[])
    })();
    match r {
        Err(e) => rejected(vm, e),
        // (PromiseResolve of the result, then to undefined.)
        Ok(v) => {
            let h = async_helper_await(vm)?;
            call_function(vm, h, Value::UNDEFINED, &[v])
        }
    }
}

/// `async (v) => { await v; }`, for `[Symbol.asyncDispose]`, made on
/// first use (with the disposal helper's roots).
fn async_helper_await(vm: &mut Vm) -> Result<Value> {
    if vm.dispose_roots.len() <= 4 {
        let f = crate::builtins::run_source(vm, "(async function (v) { await v; })")?;
        vm.dispose_roots.push(f);
    }
    Ok(vm.dispose_roots[4])
}
