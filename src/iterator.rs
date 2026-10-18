//! `Iterator` and its helpers (ES2025, ES2026's `Iterator.concat`, and the
//! stage 3 proposals' `includes`, `join`, `chunks`, `windows`,
//! `Iterator.zip` and `Iterator.zipKeyed`): the abstract constructor,
//! `Iterator.from`, and `Iterator.prototype`'s (`%IteratorPrototype%`'s)
//! methods.
//!
//! The lazy ones (`map`, `filter`, `take`, `drop`, `flatMap`, `chunks`,
//! `windows`), `Iterator.concat` and the zips make an iterator helper, `Iterator.from` a wrapper
//! for an iterator not inheriting from `Iterator.prototype`: objects of
//! class `IteratorHelper` whose `code` bits are their state, a hidden
//! array (`[kind, state, iterator, next, …]`). The spec writes a helper as
//! a generator over a closure; here its `next` runs a step of a state
//! machine per kind, and its `return` does what that generator would on a
//! return completion at its `yield`.

use crate::interp::{call_function, to_boolean, to_number};
use crate::object::{Class, DONT_DELETE, DONT_ENUM, READ_ONLY, as_object, object_value};
use crate::props::{Desc, is_callable, is_object};
use crate::symbol::{close, iter_result};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm};

// A helper's kind (its state's `KIND`).
const MAP: i32 = 0;
const FILTER: i32 = 1;
const TAKE: i32 = 2;
const DROP: i32 = 3;
const FLAT_MAP: i32 = 4;
const CONCAT: i32 = 5;
/// `Iterator.from`'s wrapper (`%WrapForValidIteratorPrototype%`'s).
const WRAP: i32 = 6;
const CHUNKS: i32 = 7;
const WINDOWS: i32 = 8;
const ZIP: i32 = 9;

// The state's cells: the kind, the generator state, the underlying
// iterator and its `next`, the function (or `concat`'s list of
// `[open method, iterable]` pairs), a counter (`take`'s and `drop`'s: the
// number left; `concat`'s: the next pair's), and an inner iterator and its
// `next` (`flatMap`'s, `concat`'s), `undefined` if none. `chunks` and
// `windows`: the size, whether a partial window is yielded, the buffer
// (an array), and whether the underlying iterator is exhausted. A zip:
// arrays of the iterators (`null` for one exhausted, in "longest" mode)
// and of their `next`s, its result's keys (`undefined`: an array), the
// mode, the padding, and an array of whether each iterator is open.
const KIND: i32 = 0;
const STATE: i32 = 1;
const ITER: i32 = 2;
const NEXT: i32 = 3;
const FUNC: i32 = 4;
const COUNT: i32 = 5;
const INNER: i32 = 6;
const INNER_NEXT: i32 = 7;

// Generator states.
const START: i32 = 0;
const SUSPENDED: i32 = 1;
const RUNNING: i32 = 2;
const DONE: i32 = 3;

/// `Vm::iterator_roots`' entries.
pub(crate) const CONSTRUCTOR: usize = 0;
const HELPER_PROTOTYPE: usize = 1;
const WRAP_PROTOTYPE: usize = 2;

pub(crate) fn install(vm: &mut Vm) {
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    let g = vm.global;
    let c = vm.define_native(g, "Iterator", iterator_ctor, 0, crate::jit::Ty::Any, true);
    let a_p = vm.intern("prototype");
    vm.add_property_with(c, a_p, object_value(ip), READ_ONLY | DONT_ENUM | DONT_DELETE);
    vm.define_natives(
        c,
        &[("from", from as NativeFn, 1), ("concat", concat, 0), ("zip", zip, 1), ("zipKeyed", zip_keyed, 1)],
    );
    vm.define_natives(
        ip,
        &[
            ("map", map as NativeFn, 1),
            ("filter", filter, 1),
            ("take", take, 1),
            ("drop", drop, 1),
            ("flatMap", flat_map, 1),
            ("reduce", reduce, 1),
            ("toArray", to_array, 0),
            ("forEach", for_each, 1),
            ("some", some, 1),
            ("every", every, 1),
            ("find", find, 1),
            ("includes", includes, 1),
            ("join", join, 1),
            ("chunks", chunks, 1),
            ("windows", windows, 1),
        ],
    );
    // `constructor` and `@@toStringTag`: accessors whose setters set the
    // property on the receiver, unless it is `Iterator.prototype` itself.
    let a_c = Key::Name(vm.intern("constructor"));
    let a_tag = Key::Name(vm.a_sym_to_string_tag);
    for (key, name, get, set) in [
        (a_c, "constructor", get_constructor as NativeFn, set_constructor as NativeFn),
        (a_tag, "[Symbol.toStringTag]", get_tag, set_tag),
    ] {
        let get = vm.native_function(&format!("get {name}"), get, 0, crate::jit::Ty::Any, false);
        let set = vm.native_function(&format!("set {name}"), set, 1, crate::jit::Ty::Any, false);
        let _ = vm.define_own_property(ip, key, &Desc::accessor(object_value(get), object_value(set), false, true));
    }

    let hp = vm.new_object(object_value(ip));
    vm.define_natives(hp, &[("next", helper_next as NativeFn, 0), ("return", helper_return, 0)]);
    crate::symbol::to_string_tag(vm, hp, "Iterator Helper");
    let wp = vm.new_object(object_value(ip));
    vm.define_natives(wp, &[("next", wrap_next as NativeFn, 0), ("return", wrap_return, 0)]);
    vm.iterator_roots = vec![c, hp, wp];
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// `Iterator()`: abstract, only a subclass's constructor's `super()`
/// constructs one (see `intrinsics::construct_with`).
fn iterator_ctor(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let derived = std::mem::replace(&mut vm.derived_construct, Value::UNDEFINED);
    if !vm.is_construct() || derived != object_value(vm.iterator_roots[CONSTRUCTOR]) {
        return Err(Error::type_error("Abstract class Iterator not directly constructable"));
    }
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    Ok(object_value(vm.new_object(object_value(ip))))
}

fn get_constructor(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(object_value(vm.iterator_roots[CONSTRUCTOR]))
}

fn get_tag(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(vm.new_string(&"Iterator".encode_utf16().collect::<Vec<_>>()))
}

fn set_constructor(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let key = Key::Name(vm.intern("constructor"));
    ignoring_prototype(vm, this, key, arg(a, 0))
}

fn set_tag(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let key = Key::Name(vm.a_sym_to_string_tag);
    ignoring_prototype(vm, this, key, arg(a, 0))
}

/// SetterThatIgnoresPrototypeProperties(this, `Iterator.prototype`, key, v).
fn ignoring_prototype(vm: &mut Vm, this: Value, key: Key, v: Value) -> Result<Value> {
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    if !is_object(this) {
        return Err(Error::type_error("Iterator.prototype setter called on a non-object"));
    }
    if as_object(this) == ip {
        return Err(Error::type_error("Cannot assign to a read-only property of Iterator.prototype"));
    }
    if vm.own_property(as_object(this), key).is_none() {
        vm.create_data_property(this, key, v)?;
    } else {
        vm.put_key(this, key, v)?;
    }
    Ok(Value::UNDEFINED)
}

// ---- The protocol ----

/// `it.next` (GetIteratorDirect's).
fn next_method(vm: &mut Vm, it: Value) -> Result<Value> {
    let a = vm.intern("next");
    vm.get(it, a)
}

/// IteratorStepValue: the next value, `None` when done.
fn step_value(vm: &mut Vm, it: Value, next: Value) -> Result<Option<Value>> {
    let r = step_result(vm, it, next)?;
    match r {
        Some(r) => {
            let a_value = vm.a_value;
            vm.get(r, a_value).map(Some)
        }
        None => Ok(None),
    }
}

/// IteratorStep: the result object, `None` when done.
fn step_result(vm: &mut Vm, it: Value, next: Value) -> Result<Option<Value>> {
    let r = call_function(vm, next, it, &[])?;
    if !is_object(r) {
        return Err(Error::type_error(format!("Iterator result {} is not an object", crate::vm::display(r))));
    }
    let a_done = vm.intern("done");
    if to_boolean(vm.get(r, a_done)?) {
        return Ok(None);
    }
    Ok(Some(r))
}

/// IteratorClose with a throw completion: `it.return()`, its own errors
/// ignored, then `e`.
fn close_throw<T>(vm: &mut Vm, it: Value, e: Error) -> Result<T> {
    let _ = close(vm, it);
    Err(e)
}

/// GetMethod(v, @@iterator).
fn iterator_method(vm: &mut Vm, v: Value) -> Result<Value> {
    let a = vm.a_sym_iterator;
    let m = vm.get(v, a)?;
    if m.is_undefined() || m.is_null() {
        return Ok(Value::UNDEFINED);
    }
    if !is_callable(m) {
        return Err(Error::type_error(format!("{} is not a function", crate::vm::display(m))));
    }
    Ok(m)
}

/// GetIteratorFlattenable(v): its iterator (`v` itself if it has no
/// `@@iterator`), and that iterator's `next`. A primitive is a TypeError
/// (but a string, with `strings`).
fn flattenable(vm: &mut Vm, v: Value, strings: bool) -> Result<(Value, Value)> {
    if !is_object(v) && !(strings && crate::vm::string_units(v).is_some()) {
        return Err(Error::type_error(format!("{} is not an object", crate::vm::display(v))));
    }
    let m = iterator_method(vm, v)?;
    let it = if m.is_undefined() { v } else { call_function(vm, m, v, &[])? };
    if !is_object(it) {
        return Err(Error::type_error(format!("{} is not an object", crate::vm::display(it))));
    }
    let next = next_method(vm, it)?;
    Ok((it, next))
}

// ---- Helpers ----

fn new_helper(vm: &mut Vm, kind: i32, it: Value, next: Value, f: Value, count: Value) -> Result<Value> {
    let state = crate::array::new_array_of(
        vm,
        &[Value::from_i32(kind), Value::from_i32(START), it, next, f, count, Value::UNDEFINED, Value::UNDEFINED],
    )?;
    let proto = vm.iterator_roots[if kind == WRAP { WRAP_PROTOTYPE } else { HELPER_PROTOTYPE }];
    let o = vm.new_object_with(Class::IteratorHelper, object_value(proto), 0);
    unsafe { (*o).code = state.to_bits() as *const crate::vm::Code };
    Ok(object_value(o))
}

/// The state of `this`, a helper (`wrap`: a wrapper), else a TypeError.
fn state_of(this: Value, wrap: bool, what: &str) -> Result<Value> {
    if this.is_cell() && unsafe { (*as_object(this)).class } == Class::IteratorHelper {
        let state = Value::from_bits(unsafe { (*as_object(this)).code } as u64);
        let kind = unsafe { *(*as_object(state)).elems };
        if (kind == Value::from_i32(WRAP)) == wrap {
            return Ok(state);
        }
    }
    Err(Error::type_error(format!("{what} called on an incompatible receiver")))
}

fn get(vm: &mut Vm, state: Value, i: i32) -> Result<Value> {
    vm.get_keyed(state, Value::from_i32(i))
}

fn set(vm: &mut Vm, state: Value, i: i32, v: Value) -> Result<()> {
    vm.set_keyed(state, Value::from_i32(i), v).map(|_| ())
}

fn int(v: Value) -> i32 {
    v.as_number().unwrap_or(0.0) as i32
}

/// `%IteratorHelperPrototype%.next()`: the next step of its closure.
fn helper_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = state_of(this, false, "Iterator Helper next")?;
    match int(get(vm, state, STATE)?) {
        RUNNING => return Err(Error::type_error("Generator is already running")),
        DONE => return iter_result(vm, Value::UNDEFINED, true),
        _ => {}
    }
    set(vm, state, STATE, Value::from_i32(RUNNING))?;
    match step(vm, state) {
        Ok(Some(v)) => {
            set(vm, state, STATE, Value::from_i32(SUSPENDED))?;
            iter_result(vm, v, false)
        }
        Ok(None) => {
            set(vm, state, STATE, Value::from_i32(DONE))?;
            iter_result(vm, Value::UNDEFINED, true)
        }
        Err(e) => {
            set(vm, state, STATE, Value::from_i32(DONE))?;
            Err(e)
        }
    }
}

/// A helper's closure run to its next `yield` (the value), or to its end.
fn step(vm: &mut Vm, state: Value) -> Result<Option<Value>> {
    let kind = int(get(vm, state, KIND)?);
    let (it, next, f) = (get(vm, state, ITER)?, get(vm, state, NEXT)?, get(vm, state, FUNC)?);
    let count = get(vm, state, COUNT)?.as_number().unwrap_or(0.0);
    match kind {
        MAP => {
            let Some(v) = step_value(vm, it, next)? else { return Ok(None) };
            let mapped = call_function(vm, f, Value::UNDEFINED, &[v, Value::number(count)]);
            let mapped = match mapped {
                Ok(m) => m,
                Err(e) => return close_throw(vm, it, e),
            };
            set(vm, state, COUNT, Value::number(count + 1.0))?;
            Ok(Some(mapped))
        }
        FILTER => {
            let mut count = count;
            loop {
                let Some(v) = step_value(vm, it, next)? else { return Ok(None) };
                let selected = match call_function(vm, f, Value::UNDEFINED, &[v, Value::number(count)]) {
                    Ok(s) => s,
                    Err(e) => return close_throw(vm, it, e),
                };
                count += 1.0;
                set(vm, state, COUNT, Value::number(count))?;
                if to_boolean(selected) {
                    return Ok(Some(v));
                }
            }
        }
        TAKE => {
            if count == 0.0 {
                close(vm, it)?;
                return Ok(None);
            }
            if count != f64::INFINITY {
                set(vm, state, COUNT, Value::number(count - 1.0))?;
            }
            step_value(vm, it, next)
        }
        DROP => {
            let mut left = count;
            while left > 0.0 {
                if left != f64::INFINITY {
                    left -= 1.0;
                    set(vm, state, COUNT, Value::number(left))?;
                }
                if step_result(vm, it, next)?.is_none() {
                    return Ok(None);
                }
            }
            step_value(vm, it, next)
        }
        FLAT_MAP => {
            let mut count = count;
            loop {
                let inner = get(vm, state, INNER)?;
                if !inner.is_undefined() {
                    let inner_next = get(vm, state, INNER_NEXT)?;
                    match step_value(vm, inner, inner_next) {
                        Ok(Some(v)) => return Ok(Some(v)),
                        Ok(None) => set(vm, state, INNER, Value::UNDEFINED)?,
                        Err(e) => return close_throw(vm, it, e),
                    }
                    continue;
                }
                let Some(v) = step_value(vm, it, next)? else { return Ok(None) };
                let flattened = call_function(vm, f, Value::UNDEFINED, &[v, Value::number(count)])
                    .and_then(|mapped| flattenable(vm, mapped, false));
                let (inner, inner_next) = match flattened {
                    Ok(r) => r,
                    Err(e) => return close_throw(vm, it, e),
                };
                count += 1.0;
                set(vm, state, COUNT, Value::number(count))?;
                set(vm, state, INNER, inner)?;
                set(vm, state, INNER_NEXT, inner_next)?;
            }
        }
        CHUNKS | WINDOWS => {
            if get(vm, state, INNER_NEXT)? == Value::TRUE {
                return Ok(None);
            }
            let buffer = get(vm, state, INNER)?;
            loop {
                let len = vm.length_of(buffer)?;
                let Some(v) = step_value(vm, it, next)? else {
                    // (`chunks`' last, or with "allow-partial" a partial
                    // window, then done.)
                    let partial = if kind == CHUNKS { true } else { count != 0.0 && len < f.as_number().unwrap_or(0.0) };
                    if len > 0.0 && partial {
                        set(vm, state, INNER_NEXT, Value::TRUE)?;
                        return copy(vm, buffer, 0.0).map(Some);
                    }
                    return Ok(None);
                };
                let size = f.as_number().unwrap_or(1.0);
                if kind == CHUNKS {
                    crate::vm::array_push(vm, buffer, &[v])?;
                    if len + 1.0 == size {
                        let out = copy(vm, buffer, 0.0)?;
                        let fresh = crate::array::new_array_of(vm, &[])?;
                        set(vm, state, INNER, fresh)?;
                        return Ok(Some(out));
                    }
                } else {
                    let buffer = if len == size { copy(vm, buffer, 1.0)? } else { buffer };
                    crate::vm::array_push(vm, buffer, &[v])?;
                    set(vm, state, INNER, buffer)?;
                    if vm.length_of(buffer)? == size {
                        return copy(vm, buffer, 0.0).map(Some);
                    }
                }
            }
        }
        ZIP => zip_step(vm, state),
        _ => {
            // `concat`: each iterable's values in turn.
            let mut i = count;
            loop {
                let inner = get(vm, state, INNER)?;
                if !inner.is_undefined() {
                    let inner_next = get(vm, state, INNER_NEXT)?;
                    match step_value(vm, inner, inner_next)? {
                        Some(v) => return Ok(Some(v)),
                        None => set(vm, state, INNER, Value::UNDEFINED)?,
                    }
                    continue;
                }
                if i >= vm.length_of(f)? / 2.0 {
                    return Ok(None);
                }
                let (m, iterable) = (vm.get_keyed(f, Value::number(2.0 * i))?, vm.get_keyed(f, Value::number(2.0 * i + 1.0))?);
                i += 1.0;
                set(vm, state, COUNT, Value::number(i))?;
                let inner = call_function(vm, m, iterable, &[])?;
                if !is_object(inner) {
                    return Err(Error::type_error(format!("{} is not an object", crate::vm::display(inner))));
                }
                let inner_next = next_method(vm, inner)?;
                set(vm, state, INNER, inner)?;
                set(vm, state, INNER_NEXT, inner_next)?;
            }
        }
    }
}

/// `%IteratorHelperPrototype%.return()`: its underlying iterator closed
/// (before it starts), or its closure resumed with a return completion
/// (the iterators open at its `yield` closed, innermost first).
fn helper_return(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = state_of(this, false, "Iterator Helper return")?;
    let kind = int(get(vm, state, KIND)?);
    match int(get(vm, state, STATE)?) {
        RUNNING => return Err(Error::type_error("Generator is already running")),
        DONE => return iter_result(vm, Value::UNDEFINED, true),
        START => {
            set(vm, state, STATE, Value::from_i32(DONE))?;
            if kind == ZIP {
                close_all(vm, state, Ok(()))?;
            } else if kind != CONCAT {
                let it = get(vm, state, ITER)?;
                close(vm, it)?;
            }
            return iter_result(vm, Value::UNDEFINED, true);
        }
        _ => {}
    }
    // (Running meanwhile: a `return()` the closing calls is an error.)
    set(vm, state, STATE, Value::from_i32(RUNNING))?;
    let (it, inner) = (get(vm, state, ITER)?, get(vm, state, INNER)?);
    let r = match kind {
        FLAT_MAP if !inner.is_undefined() => match close(vm, inner) {
            Err(e) => close_throw(vm, it, e),
            Ok(()) => close(vm, it),
        },
        CONCAT if !inner.is_undefined() => close(vm, inner),
        CONCAT => Ok(()),
        ZIP => close_all(vm, state, Ok(())),
        // (Past the underlying iterator's end: nothing to close.)
        CHUNKS | WINDOWS if get(vm, state, INNER_NEXT)? == Value::TRUE => Ok(()),
        _ => close(vm, it),
    };
    set(vm, state, STATE, Value::from_i32(DONE))?;
    r?;
    iter_result(vm, Value::UNDEFINED, true)
}

/// `%WrapForValidIteratorPrototype%.next()`: the wrapped iterator's.
fn wrap_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = state_of(this, true, "%WrapForValidIteratorPrototype%.next")?;
    let (it, next) = (get(vm, state, ITER)?, get(vm, state, NEXT)?);
    call_function(vm, next, it, &[])
}

/// `%WrapForValidIteratorPrototype%.return()`: the wrapped iterator's
/// `return()`, if it has one.
fn wrap_return(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = state_of(this, true, "%WrapForValidIteratorPrototype%.return")?;
    let it = get(vm, state, ITER)?;
    let a_return = vm.intern("return");
    let m = vm.get(it, a_return)?;
    if m.is_undefined() || m.is_null() {
        return iter_result(vm, Value::UNDEFINED, true);
    }
    call_function(vm, m, it, &[])
}

// ---- Iterator ----

/// `Iterator.from(o)`: `o`'s iterator, wrapped unless it inherits from
/// `Iterator.prototype`.
fn from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (it, next) = flattenable(vm, arg(a, 0), true)?;
    let c = object_value(vm.iterator_roots[CONSTRUCTOR]);
    if vm.instance_of(it, c)? {
        return Ok(it);
    }
    new_helper(vm, WRAP, it, next, Value::UNDEFINED, Value::UNDEFINED)
}

/// `Iterator.concat(...items)`: their values in turn (each item an
/// iterable object).
fn concat(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let list = vm.new_array();
    for &item in a {
        if !is_object(item) {
            return Err(Error::type_error(format!("{} is not an object", crate::vm::display(item))));
        }
        let m = iterator_method(vm, item)?;
        if m.is_undefined() {
            return Err(Error::type_error(format!("{} is not iterable", crate::vm::display(item))));
        }
        crate::vm::array_push(vm, list, &[m, item])?;
    }
    new_helper(vm, CONCAT, Value::UNDEFINED, Value::UNDEFINED, list, Value::from_i32(0))
}

// ---- Iterator.prototype ----

/// `this`, an object, else a TypeError.
fn this_object(this: Value, what: &str) -> Result<Value> {
    if !is_object(this) {
        return Err(Error::type_error(format!("Iterator.prototype.{what} called on a non-object")));
    }
    Ok(this)
}

/// `f`, callable, else a TypeError (iterator `it` closed).
fn callable_arg(vm: &mut Vm, it: Value, f: Value) -> Result<Value> {
    if !is_callable(f) {
        let e = Error::type_error(format!("{} is not a function", crate::vm::display(f)));
        return close_throw(vm, it, e);
    }
    Ok(f)
}

fn lazy(vm: &mut Vm, this: Value, a: &[Value], kind: i32, what: &str) -> Result<Value> {
    let it = this_object(this, what)?;
    let f = callable_arg(vm, it, arg(a, 0))?;
    let next = next_method(vm, it)?;
    new_helper(vm, kind, it, next, f, Value::from_i32(0))
}

fn map(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    lazy(vm, this, a, MAP, "map")
}

fn filter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    lazy(vm, this, a, FILTER, "filter")
}

fn flat_map(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    lazy(vm, this, a, FLAT_MAP, "flatMap")
}

/// `take(limit)`'s and `drop(limit)`'s.
fn limited(vm: &mut Vm, this: Value, a: &[Value], kind: i32, what: &str) -> Result<Value> {
    let it = this_object(this, what)?;
    let d = to_number(arg(a, 0));
    if vm.conv_thrown {
        let e = vm.take_conv_error();
        return close_throw(vm, it, e);
    }
    if d.is_nan() {
        return close_throw(vm, it, Error::range_error(format!("{} must be a number", crate::vm::display(arg(a, 0)))));
    }
    // (Infinity, or a safe integer.)
    let limit = d.trunc();
    if limit < 0.0 || limit.is_finite() && limit > 9007199254740991.0 {
        return close_throw(vm, it, Error::range_error(format!("{d} is out of range")));
    }
    let next = next_method(vm, it)?;
    new_helper(vm, kind, it, next, Value::UNDEFINED, Value::number(limit))
}

fn take(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    limited(vm, this, a, TAKE, "take")
}

fn drop(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    limited(vm, this, a, DROP, "drop")
}

/// The eager methods' loop: `f(vm, value, counter)` for each value, until
/// it gives a result (the iterator then closed), the iterator closed if it
/// fails.
fn each(
    vm: &mut Vm,
    this: Value,
    f: Option<Value>,
    what: &str,
    mut each: impl FnMut(&mut Vm, Value, f64) -> Result<Option<Value>>,
) -> Result<Option<Value>> {
    let it = this_object(this, what)?;
    if let Some(f) = f {
        callable_arg(vm, it, f)?;
    }
    let next = next_method(vm, it)?;
    let mut counter = 0.0;
    while let Some(v) = step_value(vm, it, next)? {
        match each(vm, v, counter) {
            Ok(Some(r)) => {
                close(vm, it)?;
                return Ok(Some(r));
            }
            Ok(None) => {}
            Err(e) => return close_throw(vm, it, e),
        }
        counter += 1.0;
    }
    Ok(None)
}

fn reduce(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let it = this_object(this, "reduce")?;
    let f = callable_arg(vm, it, arg(a, 0))?;
    let next = next_method(vm, it)?;
    let (mut acc, mut counter) = if a.len() >= 2 {
        (a[1], 0.0)
    } else {
        match step_value(vm, it, next)? {
            Some(v) => (v, 1.0),
            None => return Err(Error::type_error("Reduce of empty iterator with no initial value")),
        }
    };
    while let Some(v) = step_value(vm, it, next)? {
        acc = match call_function(vm, f, Value::UNDEFINED, &[acc, v, Value::number(counter)]) {
            Ok(r) => r,
            Err(e) => return close_throw(vm, it, e),
        };
        counter += 1.0;
    }
    Ok(acc)
}

fn to_array(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let out = vm.new_array();
    each(vm, this, None, "toArray", |vm, v, _| crate::vm::array_push(vm, out, &[v]).map(|_| None))?;
    Ok(out)
}

fn for_each(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let f = arg(a, 0);
    each(vm, this, Some(f), "forEach", |vm, v, k| call_function(vm, f, Value::UNDEFINED, &[v, Value::number(k)]).map(|_| None))?;
    Ok(Value::UNDEFINED)
}

/// `some`'s, `every`'s and `find`'s: the first value `f` says `want` of.
fn search(vm: &mut Vm, this: Value, a: &[Value], want: bool, what: &str) -> Result<Option<Value>> {
    let f = arg(a, 0);
    each(vm, this, Some(f), what, |vm, v, k| {
        let r = call_function(vm, f, Value::UNDEFINED, &[v, Value::number(k)])?;
        Ok((to_boolean(r) == want).then_some(v))
    })
}

fn some(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(search(vm, this, a, true, "some")?.is_some()))
}

fn every(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(search(vm, this, a, false, "every")?.is_none()))
}

fn find(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    Ok(search(vm, this, a, true, "find")?.unwrap_or(Value::UNDEFINED))
}


/// A copy of array `a` from index `from` on (`CreateArrayFromList`).
fn copy(vm: &mut Vm, a: Value, from: f64) -> Result<Value> {
    let out = vm.new_array();
    let len = vm.length_of(a)?;
    let mut i = from;
    while i < len {
        let v = vm.get_keyed(a, Value::number(i))?;
        crate::vm::array_push(vm, out, &[v])?;
        i += 1.0;
    }
    Ok(out)
}

/// Whether `v` is the string `s`.
fn is_string(v: Value, s: &str) -> bool {
    crate::vm::string_units(v).is_some_and(|u| u.iter().copied().eq(s.encode_utf16()))
}

/// `includes(searchElement, skippedElements)`: whether a value past the
/// first `skippedElements` is `searchElement` (SameValueZero).
fn includes(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let it = this_object(this, "includes")?;
    let (search, skip) = (arg(a, 0), arg(a, 1));
    let to_skip = if skip.is_undefined() {
        0.0
    } else {
        match skip.as_number() {
            Some(d) if d.is_infinite() || d.fract() == 0.0 => d,
            _ => return close_throw(vm, it, Error::type_error("skippedElements must be an integral number")),
        }
    };
    if to_skip < 0.0 || to_skip.is_finite() && to_skip > 9007199254740991.0 {
        return close_throw(vm, it, Error::range_error("skippedElements is out of range"));
    }
    let mut skipped = 0.0;
    let found = each(vm, it, None, "includes", |_, v, _| {
        if skipped < to_skip {
            skipped += 1.0;
            return Ok(None);
        }
        Ok(crate::collections::same_value_zero(v, search).then_some(Value::TRUE))
    })?;
    Ok(Value::from_bool(found.is_some()))
}

/// `join(separator)`: the values as strings (`null` and `undefined` as
/// empty ones), `separator` (`","` if undefined) between them.
fn join(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let it = this_object(this, "join")?;
    let sep: Vec<u16> = match arg(a, 0) {
        s if s.is_undefined() => vec![b',' as u16],
        s => match crate::intrinsics::to_string(vm, s) {
            Ok(s) => crate::vm::string_units(s).unwrap_or(&[]).to_vec(),
            Err(e) => return close_throw(vm, it, e),
        },
    };
    let mut out: Vec<u16> = Vec::new();
    let mut first = true;
    each(vm, it, None, "join", |vm, v, _| {
        if !first {
            out.extend_from_slice(&sep);
        }
        first = false;
        if !v.is_undefined() && !v.is_null() {
            let s = crate::intrinsics::to_string(vm, v)?;
            out.extend_from_slice(crate::vm::string_units(s).unwrap_or(&[]));
        }
        Ok(None)
    })?;
    Ok(vm.new_string(&out))
}

/// `chunks(size)`'s and `windows(size, undersized)`'s size: an integral
/// number from 1 to 2^32 - 1 (iterator `it` closed if not).
fn size_arg(vm: &mut Vm, it: Value, v: Value) -> Result<f64> {
    match v.as_number() {
        Some(d) if d.is_finite() && d.fract() == 0.0 => {
            if !(1.0..=4294967295.0).contains(&d) {
                return close_throw(vm, it, Error::range_error(format!("{d} is out of range")));
            }
            Ok(d)
        }
        _ => close_throw(vm, it, Error::type_error(format!("{} is not an integral number", crate::vm::display(v)))),
    }
}

/// `chunks(size)`: arrays of `size` values in turn (the last maybe fewer).
fn chunks(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let it = this_object(this, "chunks")?;
    let size = size_arg(vm, it, arg(a, 0))?;
    let next = next_method(vm, it)?;
    let h = new_helper(vm, CHUNKS, it, next, Value::number(size), Value::from_i32(0))?;
    let state = Value::from_bits(unsafe { (*as_object(h)).code } as u64);
    let buffer = crate::array::new_array_of(vm, &[])?;
    set(vm, state, INNER, buffer)?;
    set(vm, state, INNER_NEXT, Value::FALSE)?;
    Ok(h)
}

/// `windows(size, undersized)`: arrays of each `size` consecutive values
/// (with "allow-partial", all of them if fewer).
fn windows(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let it = this_object(this, "windows")?;
    let size = size_arg(vm, it, arg(a, 0))?;
    let undersized = arg(a, 1);
    let partial = if undersized.is_undefined() || is_string(undersized, "only-full") {
        false
    } else if is_string(undersized, "allow-partial") {
        true
    } else {
        return close_throw(vm, it, Error::type_error("undersized must be \"only-full\" or \"allow-partial\""));
    };
    let next = next_method(vm, it)?;
    let h = new_helper(vm, WINDOWS, it, next, Value::number(size), Value::from_i32(partial as i32))?;
    let state = Value::from_bits(unsafe { (*as_object(h)).code } as u64);
    let buffer = crate::array::new_array_of(vm, &[])?;
    set(vm, state, INNER, buffer)?;
    set(vm, state, INNER_NEXT, Value::FALSE)?;
    Ok(h)
}

// ---- Iterator.zip ----

// A zip's modes.
const SHORTEST: i32 = 0;
const LONGEST: i32 = 1;
const STRICT: i32 = 2;

/// The zips' options: the mode, and (in "longest" mode) the padding
/// option.
fn zip_options(vm: &mut Vm, options: Value) -> Result<(i32, Value)> {
    if options.is_undefined() {
        return Ok((SHORTEST, Value::UNDEFINED));
    }
    if !is_object(options) {
        return Err(Error::type_error("options must be an object"));
    }
    let a_mode = vm.intern("mode");
    let m = vm.get(options, a_mode)?;
    let mode = if m.is_undefined() || is_string(m, "shortest") {
        SHORTEST
    } else if is_string(m, "longest") {
        LONGEST
    } else if is_string(m, "strict") {
        STRICT
    } else {
        return Err(Error::type_error("mode must be \"shortest\", \"longest\" or \"strict\""));
    };
    let mut padding = Value::UNDEFINED;
    if mode == LONGEST {
        let a_padding = vm.intern("padding");
        padding = vm.get(options, a_padding)?;
        if !padding.is_undefined() && !is_object(padding) {
            return Err(Error::type_error("padding must be an object"));
        }
    }
    Ok((mode, padding))
}

/// IteratorCloseAll of the iterators in `iters` (in reverse order), with
/// completion `c`.
fn close_list(vm: &mut Vm, iters: &[Value], mut c: Result<()>) -> Result<()> {
    for &it in iters.iter().rev() {
        c = match c {
            Ok(()) => close(vm, it),
            Err(e) => close_throw(vm, it, e),
        };
    }
    c
}

/// A zip helper of `iters` (with their `next`s), its results arrays or
/// (with `keys`) objects of those keys.
fn new_zip(vm: &mut Vm, iters: &[(Value, Value)], keys: Value, mode: i32, padding: &[Value]) -> Result<Value> {
    let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
    let nexts: Vec<Value> = iters.iter().map(|p| p.1).collect();
    let (its, nexts) = (crate::array::new_array_of(vm, &its)?, crate::array::new_array_of(vm, &nexts)?);
    let h = new_helper(vm, ZIP, its, nexts, keys, Value::from_i32(mode))?;
    let state = Value::from_bits(unsafe { (*as_object(h)).code } as u64);
    let padding = crate::array::new_array_of(vm, padding)?;
    let open = crate::array::new_array_of(vm, &vec![Value::TRUE; iters.len()])?;
    set(vm, state, INNER, padding)?;
    set(vm, state, INNER_NEXT, open)?;
    Ok(h)
}

/// "longest" mode's padding: `n` values, from iterable `option` (or
/// `undefined`s), or with `keys` its properties of those keys (the
/// iterators closed on an error).
fn zip_padding(vm: &mut Vm, option: Value, keys: Option<&[Key]>, iters: &[(Value, Value)]) -> Result<Vec<Value>> {
    let n = iters.len();
    let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
    if option.is_undefined() {
        return Ok(vec![Value::UNDEFINED; n]);
    }
    let mut out = Vec::with_capacity(n);
    if let Some(keys) = keys {
        for &k in keys {
            match vm.get_key(option, k) {
                Ok(v) => {
                    vm.handles.push(v);
                    out.push(v);
                }
                Err(e) => return close_list(vm, &its, Err(e)).map(|()| Vec::new()),
            }
        }
        return Ok(out);
    }
    let r = (|| {
        let p = crate::symbol::get_iterator(vm, option)?;
        let next = next_method(vm, p)?;
        Ok((p, next))
    })();
    let (p, next) = match r {
        Ok(r) => r,
        Err(e) => return close_list(vm, &its, Err(e)).map(|()| Vec::new()),
    };
    let mut using = true;
    for _ in 0..n {
        if using {
            match step_value(vm, p, next) {
                Ok(Some(v)) => {
                    vm.handles.push(v);
                    out.push(v);
                }
                Ok(None) => using = false,
                Err(e) => return close_list(vm, &its, Err(e)).map(|()| Vec::new()),
            }
        }
        if !using {
            out.push(Value::UNDEFINED);
        }
    }
    if using && let Err(e) = close(vm, p) {
        return close_list(vm, &its, Err(e)).map(|()| Vec::new());
    }
    Ok(out)
}

/// `Iterator.zip(iterables, options)`: arrays of each iterable's next
/// values, until the shortest ends ("longest": the longest, padded;
/// "strict": all at once, or a TypeError).
fn zip(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let iterables = arg(a, 0);
    if !is_object(iterables) {
        return Err(Error::type_error("Iterator.zip: iterables must be an object"));
    }
    let (mode, padding) = zip_options(vm, arg(a, 1))?;
    let input = crate::symbol::get_iterator(vm, iterables)?;
    let input_next = next_method(vm, input)?;
    vm.handle_scope(|vm| zip_iterables(vm, input, input_next, mode, padding))
}

fn zip_iterables(vm: &mut Vm, input: Value, input_next: Value, mode: i32, padding: Value) -> Result<Value> {
    let mut iters: Vec<(Value, Value)> = Vec::new();
    loop {
        let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
        let v = match step_value(vm, input, input_next) {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(e) => return close_list(vm, &its, Err(e)).map(|()| Value::UNDEFINED),
        };
        match flattenable(vm, v, false) {
            Ok(p) => {
                vm.handles.extend([p.0, p.1]);
                iters.push(p);
            }
            Err(e) => {
                let mut all = vec![input];
                all.extend(its);
                // (`input` first: IteratorCloseAll closes in reverse.)
                return close_list(vm, &all, Err(e)).map(|()| Value::UNDEFINED);
            }
        }
    }
    let padding = if mode == LONGEST { zip_padding(vm, padding, None, &iters)? } else { Vec::new() };
    new_zip(vm, &iters, Value::UNDEFINED, mode, &padding)
}

/// `Iterator.zipKeyed(iterables, options)`: as `Iterator.zip` over the
/// object `iterables`' enumerable own properties not undefined, its
/// results objects of their keys.
fn zip_keyed(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let iterables = arg(a, 0);
    if !is_object(iterables) {
        return Err(Error::type_error("Iterator.zipKeyed: iterables must be an object"));
    }
    let (mode, padding) = zip_options(vm, arg(a, 1))?;
    let all = crate::proxy::own_keys(vm, as_object(iterables))?;
    vm.handle_scope(|vm| {
        let mut keys = Vec::new();
        let mut iters: Vec<(Value, Value)> = Vec::new();
        for key in all {
            let r = (|| {
                let Some(d) = crate::proxy::get_own_property(vm, as_object(iterables), key)? else { return Ok(None) };
                if d.enumerable != Some(true) {
                    return Ok(None);
                }
                let v = vm.get_key(iterables, key)?;
                if v.is_undefined() {
                    return Ok(None);
                }
                flattenable(vm, v, false).map(Some)
            })();
            match r {
                Ok(Some(p)) => {
                    vm.handles.extend([p.0, p.1]);
                    keys.push(key);
                    iters.push(p);
                }
                Ok(None) => {}
                Err(e) => {
                    let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
                    return close_list(vm, &its, Err(e)).map(|()| Value::UNDEFINED);
                }
            }
        }
        let padding = if mode == LONGEST { zip_padding(vm, padding, Some(&keys), &iters)? } else { Vec::new() };
        let names: Vec<Value> = keys
            .iter()
            .map(|&k| {
                let s = vm.key_value(k);
                if s.is_number() { vm.key_string(k) } else { s }
            })
            .collect();
        vm.handles.extend(names.iter().copied());
        let keys = crate::array::new_array_of(vm, &names)?;
        new_zip(vm, &iters, keys, mode, &padding)
    })
}

/// A zip's open iterators, closed with completion `c` (IteratorCloseAll).
fn close_all(vm: &mut Vm, state: Value, c: Result<()>) -> Result<()> {
    let (iters, open) = (get(vm, state, ITER)?, get(vm, state, INNER_NEXT)?);
    let n = vm.length_of(iters)? as usize;
    let mut its = Vec::new();
    for i in 0..n {
        if vm.get_keyed(open, Value::from_i32(i as i32))? == Value::TRUE {
            its.push(vm.get_keyed(iters, Value::from_i32(i as i32))?);
        }
    }
    close_list(vm, &its, c)
}

/// A zip's next results (IteratorZip's closure, to its `yield`).
fn zip_step(vm: &mut Vm, state: Value) -> Result<Option<Value>> {
    vm.handle_scope(|vm| zip_results(vm, state))
}

fn zip_results(vm: &mut Vm, state: Value) -> Result<Option<Value>> {
    let (iters, nexts, keys) = (get(vm, state, ITER)?, get(vm, state, NEXT)?, get(vm, state, FUNC)?);
    let (mode, padding, open) = (int(get(vm, state, COUNT)?), get(vm, state, INNER)?, get(vm, state, INNER_NEXT)?);
    let n = vm.length_of(iters)? as i32;
    if n == 0 {
        return Ok(None);
    }
    let shut = |vm: &mut Vm, i: i32| vm.set_keyed(open, Value::from_i32(i), Value::FALSE).map(|_| ());
    let mismatch = || Error::type_error("Iterator.zip: the iterators' lengths differ (strict mode)");
    let mut results = Vec::with_capacity(n as usize);
    for i in 0..n {
        let it = vm.get_keyed(iters, Value::from_i32(i))?;
        if it.is_null() {
            let v = vm.get_keyed(padding, Value::from_i32(i))?;
            vm.handles.push(v);
            results.push(v);
            continue;
        }
        let next = vm.get_keyed(nexts, Value::from_i32(i))?;
        match step_value(vm, it, next) {
            Ok(Some(v)) => {
                vm.handles.push(v);
                results.push(v);
            }
            Err(e) => {
                shut(vm, i)?;
                return close_all(vm, state, Err(e)).map(|()| None);
            }
            Ok(None) => {
                shut(vm, i)?;
                match mode {
                    SHORTEST => return close_all(vm, state, Ok(())).map(|()| None),
                    STRICT => {
                        if i != 0 {
                            return close_all(vm, state, Err(mismatch())).map(|()| None);
                        }
                        for k in 1..n {
                            let (it, next) = (vm.get_keyed(iters, Value::from_i32(k))?, vm.get_keyed(nexts, Value::from_i32(k))?);
                            match step_result(vm, it, next) {
                                Err(e) => {
                                    shut(vm, k)?;
                                    return close_all(vm, state, Err(e)).map(|()| None);
                                }
                                Ok(None) => shut(vm, k)?,
                                Ok(Some(_)) => return close_all(vm, state, Err(mismatch())).map(|()| None),
                            }
                        }
                        return Ok(None);
                    }
                    _ => {
                        let mut any = false;
                        for k in 0..n {
                            any |= vm.get_keyed(open, Value::from_i32(k))? == Value::TRUE;
                        }
                        if !any {
                            return Ok(None);
                        }
                        vm.set_keyed(iters, Value::from_i32(i), Value::NULL)?;
                        let v = vm.get_keyed(padding, Value::from_i32(i))?;
                        vm.handles.push(v);
                        results.push(v);
                    }
                }
            }
        }
    }
    if keys.is_undefined() {
        return crate::array::new_array_of(vm, &results).map(Some);
    }
    let o = object_value(vm.new_object(Value::NULL));
    for (i, v) in results.into_iter().enumerate() {
        let k = vm.get_keyed(keys, Value::from_i32(i as i32))?;
        let key = crate::intrinsics::to_property_key(vm, k)?;
        vm.create_data_property(o, key, v)?;
    }
    Ok(Some(o))
}
