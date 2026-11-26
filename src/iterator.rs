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

use crate::brand::{Args, Fresh, Scope};
use crate::interp::{to_boolean, to_number};
use crate::object::{Class, DONT_DELETE, DONT_ENUM, Obj, READ_ONLY, as_object, object_value};
use crate::props::{Desc, is_callable, is_object};
use crate::raw::ObjectPtr;
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
    let g = vm.global_obj();
    let c = vm.define_native(g, "Iterator", iterator_ctor, 0, crate::jit::Ty::Any, true);
    let a_p = vm.intern_name("prototype");
    vm.add_property_with(c, a_p, object_value(ip.ptr()), READ_ONLY | DONT_ENUM | DONT_DELETE);
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
    let a_c = Key::Name(vm.intern_name("constructor"));
    let a_tag = Key::Name(vm.a_sym_to_string_tag);
    for (key, name, get, set) in [
        (a_c, "constructor", get_constructor as NativeFn, set_constructor as NativeFn),
        (a_tag, "[Symbol.toStringTag]", get_tag, set_tag),
    ] {
        let get = vm.native_function(&format!("get {name}"), get, 0, crate::jit::Ty::Any, false);
        let set = vm.native_function(&format!("set {name}"), set, 1, crate::jit::Ty::Any, false);
        let _ = vm.define_own_property(
            ip,
            key,
            &Desc::accessor(object_value(get.ptr()), object_value(set.ptr()), false, true),
        );
    }

    // raw: only calls that don't collect follow (allocation, stores, filling it).
    let hp = vm.mu().new_plain(object_value(ip.ptr())).raw_obj();
    vm.define_natives(hp.v(), &[("next", helper_next as NativeFn, 0), ("return", helper_return, 0)]);
    crate::symbol::to_string_tag(vm, hp.v(), "Iterator Helper");
    // raw: only calls that don't collect follow (allocation, stores, the VM's fields).
    let wp = vm.mu().new_plain(object_value(ip.ptr())).raw_obj();
    vm.define_natives(wp.v(), &[("next", wrap_next as NativeFn, 0), ("return", wrap_return, 0)]);
    vm.iterator_roots = vec![c, hp.v(), wp.v()];
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// `Iterator()`: abstract, only a subclass's constructor's `super()`
/// constructs one (see `intrinsics::construct_with`).
fn iterator_ctor(vm: &mut Vm, _: Value, _: Args<'_>) -> Result<Value> {
    let derived = std::mem::replace(&mut vm.derived_construct, Value::UNDEFINED);
    if !vm.is_construct() || derived != object_value((vm.iterator_roots[CONSTRUCTOR]).ptr()) {
        return Err(Error::type_error("Abstract class Iterator not directly constructable"));
    }
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    Ok(vm.mu().new_plain(object_value(ip.ptr())).v())
}

fn get_constructor(vm: &mut Vm, _: Value, _: Args<'_>) -> Result<Value> {
    Ok(object_value((vm.iterator_roots[CONSTRUCTOR]).ptr()))
}

fn get_tag(vm: &mut Vm, _: Value, _: Args<'_>) -> Result<Value> {
    Ok(vm.mu().new_string(&"Iterator".encode_utf16().collect::<Vec<_>>()).v())
}

fn set_constructor(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let key = Key::Name(vm.intern_name("constructor"));
    ignoring_prototype(vm, this, key, arg(&a, 0)).map(Fresh::take)
}

fn set_tag(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let key = Key::Name(vm.a_sym_to_string_tag);
    ignoring_prototype(vm, this, key, arg(&a, 0)).map(Fresh::take)
}

/// SetterThatIgnoresPrototypeProperties(this, `Iterator.prototype`, key, v).
#[inline(always)]
fn ignoring_prototype(vm: &mut Vm, this: Value, key: Key, v: Value) -> Result<Fresh<'_>> {
    ignoring_prototype_raw(vm, this, key, v).map(Fresh::new)
}

fn ignoring_prototype_raw(vm: &mut Vm, this: Value, key: Key, v: Value) -> Result<Value> {
    let ip = vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize];
    if !is_object(this) {
        return Err(Error::type_error("Iterator.prototype setter called on a non-object"));
    }
    if as_object(this) == ip.ptr() {
        return Err(Error::type_error("Cannot assign to a read-only property of Iterator.prototype"));
    }
    if vm.own_property(Obj::of(this), key).is_none() {
        vm.create_data_property(this, key, v)?;
    } else {
        vm.put_key(this, key, v)?;
    }
    Ok(Value::UNDEFINED)
}

// ---- The protocol ----

/// `it.next` (GetIteratorDirect's).
#[inline(always)]
fn next_method(vm: &mut Vm, it: Value) -> Result<Fresh<'_>> {
    let a = vm.intern_name("next");
    vm.get_atom_b(it, a)
}

/// IteratorStepValue: the next value, `None` when done.
#[inline(always)]
fn step_value(vm: &mut Vm, it: Value, next: Value) -> Result<Fresh<'_, Option<Value>>> {
    step_value_raw(vm, it, next).map(Fresh::new)
}

fn step_value_raw(vm: &mut Vm, it: Value, next: Value) -> Result<Option<Value>> {
    // (`r`, the call's result only, the receiver after a proxy's handler
    // ran: rooted.)
    let sc = Scope::new(vm);
    match step_result(vm, it, next)?.root(&sc) {
        Some(r) => {
            let a_value = vm.a_value;
            vm.get_atom_b(r.v(), a_value).map(|v| Some(v.take()))
        }
        None => Ok(None),
    }
}

/// IteratorStep: the result object, `None` when done.
#[inline(always)]
fn step_result(vm: &mut Vm, it: Value, next: Value) -> Result<Fresh<'_, Option<Value>>> {
    step_result_raw(vm, it, next).map(Fresh::new)
}

fn step_result_raw(vm: &mut Vm, it: Value, next: Value) -> Result<Option<Value>> {
    let sc_r = Scope::new(vm);
    let r = vm.call_b(next, it, &[])?.root(&sc_r);
    if !is_object(r.peek()) {
        return Err(Error::type_error(format!("Iterator result {} is not an object", crate::vm::shown(r.peek()))));
    }
    let a_done = vm.intern_name("done");
    // (`r`, the call's result only, the receiver after a proxy's handler
    // ran: rooted, above.)
    if to_boolean(vm.get_atom_b(r.peek(), a_done)?.take()) {
        return Ok(None);
    }
    Ok(Some(r.peek()))
}

/// IteratorClose with a throw completion: `it.return()`, its own errors
/// ignored, then `e`.
fn close_throw<T>(vm: &mut Vm, it: Value, e: Error) -> Result<T> {
    // (What was thrown, held across `return()`: rooted.)
    let _ = {
        let held = Scope::new(vm);
        let _held = held.roots([e.1.unwrap_or(Value::UNDEFINED)]);
        close(vm, it)
    };
    Err(e)
}

/// GetMethod(v, @@iterator).
#[inline(always)]
fn iterator_method(vm: &mut Vm, v: Value) -> Result<Fresh<'_>> {
    iterator_method_raw(vm, v).map(Fresh::new)
}

fn iterator_method_raw(vm: &mut Vm, v: Value) -> Result<Value> {
    let a = vm.a_sym_iterator;
    let m = vm.get_atom_b(v, a)?;
    if m.peek().is_undefined() || m.peek().is_null() {
        return Ok(Value::UNDEFINED);
    }
    if !is_callable(m.peek()) {
        return Err(Error::type_error(format!("{} is not a function", crate::vm::shown(m.peek()))));
    }
    Ok(m.peek())
}

/// GetIteratorFlattenable(v): its iterator (`v` itself if it has no
/// `@@iterator`), and that iterator's `next`. A primitive is a TypeError
/// (but a string, with `strings`).
#[inline(always)]
fn flattenable(vm: &mut Vm, v: Value, strings: bool) -> Result<Fresh<'_, (Value, Value)>> {
    flattenable_raw(vm, v, strings).map(Fresh::new)
}

fn flattenable_raw(vm: &mut Vm, v: Value, strings: bool) -> Result<(Value, Value)> {
    if !is_object(v) && !(strings && crate::vm::is_string(v)) {
        return Err(Error::type_error(format!("{} is not an object", crate::vm::shown(v))));
    }
    // (`v`, maybe a call's result only, held across its `@@iterator`'s
    // read; `it` across its `next`'s: rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([v]);
    let sc_m = Scope::new(vm);
    let m = iterator_method(vm, v)?.root(&sc_m);
    let it = if m.peek().is_undefined() { v } else { vm.call_b(m.peek(), v, &[])?.take() };
    if !is_object(it) {
        return Err(Error::type_error(format!("{} is not an object", crate::vm::shown(it))));
    }
    let next = {
        let held = Scope::new(vm);
        let _held = held.roots([it]);
        next_method(vm, it)
    }?;
    Ok((it, next.peek()))
}

// ---- Helpers ----

#[inline(always)]
fn new_helper(vm: &mut Vm, kind: i32, it: Value, next: Value, f: Value, count: Value) -> Result<Fresh<'_>> {
    new_helper_raw(vm, kind, it, next, f, count).map(Fresh::new)
}

fn new_helper_raw(vm: &mut Vm, kind: i32, it: Value, next: Value, f: Value, count: Value) -> Result<Value> {
    // raw: only allocation follows until the helper holds it.
    let state = crate::array::new_array_of(
        vm,
        &[Value::from_i32(kind), Value::from_i32(START), it, next, f, count, Value::UNDEFINED, Value::UNDEFINED],
    )?
    .raw();
    let proto = vm.iterator_roots[if kind == WRAP { WRAP_PROTOTYPE } else { HELPER_PROTOTYPE }];
    let o = vm.mu().new_object_with(Class::IteratorHelper, object_value(proto.ptr()), 0);
    unsafe { (*o.obj().ptr()).code.init(state.v().to_bits() as *const crate::vm::Code) };
    Ok(object_value(o.obj().ptr()))
}

/// The state of `this`, a helper (`wrap`: a wrapper), else a TypeError.
fn state_of(this: Value, wrap: bool, what: &str) -> Result<Value> {
    if this.is_cell() && Obj::of(this).class() == Class::IteratorHelper {
        let state = Value::from_bits(Obj::of(this).code() as u64);
        let kind = unsafe { *Obj::of(state).elems() };
        if (kind == Value::from_i32(WRAP)) == wrap {
            return Ok(state);
        }
    }
    Err(Error::type_error(format!("{what} called on an incompatible receiver")))
}
fn get(vm: &mut Vm, state: Value, i: i32) -> Result<Value> {
    vm.get_keyed_b(state, Value::from_i32(i)).map(Fresh::take)
}

fn set(vm: &mut Vm, state: Value, i: i32, v: Value) -> Result<()> {
    vm.set_keyed(state, Value::from_i32(i), v).map(|_| ())
}

fn int(v: Value) -> i32 {
    v.as_number().unwrap_or(0.0) as i32
}

/// `%IteratorHelperPrototype%.next()`: the next step of its closure.
fn helper_next(vm: &mut Vm, this: Value, _: Args<'_>) -> Result<Value> {
    let state = state_of(this, false, "Iterator Helper next")?;
    match int(get(vm, state, STATE)?) {
        RUNNING => return Err(Error::type_error("Generator is already running")),
        DONE => return iter_result(vm, Value::UNDEFINED, true).map(Fresh::take),
        _ => {}
    }
    set(vm, state, STATE, Value::from_i32(RUNNING))?;
    // take: given to iter_result, which only allocates, after a store into
    // the helper's own state, which runs no JS.
    match step(vm, state).map(Fresh::take) {
        Ok(Some(v)) => {
            set(vm, state, STATE, Value::from_i32(SUSPENDED))?;
            iter_result(vm, v, false).map(Fresh::take)
        }
        Ok(None) => {
            set(vm, state, STATE, Value::from_i32(DONE))?;
            iter_result(vm, Value::UNDEFINED, true).map(Fresh::take)
        }
        Err(e) => {
            set(vm, state, STATE, Value::from_i32(DONE))?;
            Err(e)
        }
    }
}

/// A helper's closure run to its next `yield` (the value), or to its end.
#[inline(always)]
fn step(vm: &mut Vm, state: Value) -> Result<Fresh<'_, Option<Value>>> {
    step_raw(vm, state).map(Fresh::new)
}

fn step_raw(vm: &mut Vm, state: Value) -> Result<Option<Value>> {
    let kind = int(get(vm, state, KIND)?);
    let (it, next, f) = (get(vm, state, ITER)?, get(vm, state, NEXT)?, get(vm, state, FUNC)?);
    let count = get(vm, state, COUNT)?.as_number().unwrap_or(0.0);
    match kind {
        MAP => {
            // take: given at once to the call, which roots its arguments.
            let Some(v) = step_value(vm, it, next)?.take() else { return Ok(None) };
            // take: returned; set stores into the helper's own state, which runs no JS.
            let mapped = vm.call_b(f, Value::UNDEFINED, &[v, Value::number(count)]).map(Fresh::take);
            let mapped = match mapped {
                Ok(m) => m,
                Err(e) => return close_throw(vm, it, e),
            };
            set(vm, state, COUNT, Value::number(count + 1.0))?;
            Ok(Some(mapped))
        }
        FILTER => {
            let mut count = count;
            // (Each value, the result if selected, rooted across the
            // predicate: its argument slot it may overwrite.)
            let sc = Scope::new(vm);
            let held = sc.root(Value::UNDEFINED);
            loop {
                // take: rooted on the next line.
                let Some(v) = step_value(vm, it, next)?.take() else { return Ok(None) };
                held.set(v);
                let selected = match vm.call_b(f, Value::UNDEFINED, &[v, Value::number(count)]) {
                    Ok(s) => s.take(),
                    Err(e) => return close_throw(vm, it, e),
                };
                count += 1.0;
                set(vm, state, COUNT, Value::number(count))?;
                if to_boolean(selected) {
                    return Ok(Some(held.v()));
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
            step_value(vm, it, next).map(Fresh::take)
        }
        DROP => {
            let mut left = count;
            while left > 0.0 {
                if left != f64::INFINITY {
                    left -= 1.0;
                    set(vm, state, COUNT, Value::number(left))?;
                }
                if step_result(vm, it, next)?.peek().is_none() {
                    return Ok(None);
                }
            }
            step_value(vm, it, next).map(Fresh::take)
        }
        FLAT_MAP => {
            let mut count = count;
            loop {
                let inner = get(vm, state, INNER)?;
                if !inner.is_undefined() {
                    let inner_next = get(vm, state, INNER_NEXT)?;
                    match step_value(vm, inner, inner_next).map(Fresh::transpose) {
                        Ok(Some(v)) => return Ok(Some(v.peek())),
                        Ok(None) => set(vm, state, INNER, Value::UNDEFINED)?,
                        Err(e) => return close_throw(vm, it, e),
                    }
                    continue;
                }
                // take: given at once to the call, which roots its arguments.
                let Some(v) = step_value(vm, it, next)?.take() else { return Ok(None) };
                // take: given at once to flattenable, which roots it.
                let mapped = vm.call_b(f, Value::UNDEFINED, &[v, Value::number(count)]).map(Fresh::take);
                let flattened = match mapped {
                    Ok(mapped) => flattenable(vm, mapped, false).map(Fresh::take),
                    Err(e) => Err(e),
                };
                // take: stored into the helper's own state, which runs no JS.
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
                let sc_v = Scope::new(vm);
                let Some(v) = step_value(vm, it, next)?.root(&sc_v) else {
                    // (`chunks`' last, or with "allow-partial" a partial
                    // window, then done.)
                    let partial =
                        if kind == CHUNKS { true } else { count != 0.0 && len < f.as_number().unwrap_or(0.0) };
                    if len > 0.0 && partial {
                        set(vm, state, INNER_NEXT, Value::TRUE)?;
                        return copy(vm, buffer, 0.0).map(|c| Some(c.take()));
                    }
                    return Ok(None);
                };
                let size = f.as_number().unwrap_or(1.0);
                if kind == CHUNKS {
                    crate::vm::array_push(vm, buffer, &[v.v()])?;
                    if len + 1.0 == size {
                        let sc_out = Scope::new(vm);
                        let out = copy(vm, buffer, 0.0)?.root(&sc_out);
                        // take: stored in the helper's state at once.
                        let fresh = crate::array::new_array_of(vm, &[])?.take();
                        set(vm, state, INNER, fresh)?;
                        return Ok(Some(out.v()));
                    }
                } else {
                    let sc_b = Scope::new(vm);
                    let buffer = if len == size { copy(vm, buffer, 1.0)?.root(&sc_b).v() } else { buffer };
                    crate::vm::array_push(vm, buffer, &[v.v()])?;
                    set(vm, state, INNER, buffer)?;
                    if vm.length_of(buffer)? == size {
                        return copy(vm, buffer, 0.0).map(|c| Some(c.take()));
                    }
                }
            }
        }
        ZIP => zip_step(vm, state).map(Fresh::take),
        _ => {
            // `concat`: each iterable's values in turn.
            let mut i = count;
            loop {
                let inner = get(vm, state, INNER)?;
                if !inner.is_undefined() {
                    let inner_next = get(vm, state, INNER_NEXT)?;
                    match step_value(vm, inner, inner_next)?.transpose() {
                        Some(v) => return Ok(Some(v.peek())),
                        None => set(vm, state, INNER, Value::UNDEFINED)?,
                    }
                    continue;
                }
                if i >= vm.length_of(f)? / 2.0 {
                    return Ok(None);
                }
                let (m, iterable) = (
                    vm.get_keyed_b(f, Value::number(2.0 * i))?.take(),
                    vm.get_keyed_b(f, Value::number(2.0 * i + 1.0))?.take(),
                );
                i += 1.0;
                set(vm, state, COUNT, Value::number(i))?;
                let sc_inner = Scope::new(vm);
                let inner = vm.call_b(m, iterable, &[])?.root(&sc_inner);
                if !is_object(inner.peek()) {
                    return Err(Error::type_error(format!("{} is not an object", crate::vm::shown(inner.peek()))));
                }
                // (`inner`, the call's result only, held across its
                // `next`'s read: rooted.)
                {
                    let sc = Scope::new(vm);
                    let inner = sc.root(inner.peek());
                    let sc_inner_next = Scope::new(vm);
                    let inner_next = next_method(vm, inner.peek())?.root(&sc_inner_next);
                    set(vm, state, INNER, inner.peek())?;
                    set(vm, state, INNER_NEXT, inner_next.peek())?;
                }
            }
        }
    }
}

/// `%IteratorHelperPrototype%.return()`: its underlying iterator closed
/// (before it starts), or its closure resumed with a return completion
/// (the iterators open at its `yield` closed, innermost first).
fn helper_return(vm: &mut Vm, this: Value, _: Args<'_>) -> Result<Value> {
    let state = state_of(this, false, "Iterator Helper return")?;
    let kind = int(get(vm, state, KIND)?);
    match int(get(vm, state, STATE)?) {
        RUNNING => return Err(Error::type_error("Generator is already running")),
        DONE => return iter_result(vm, Value::UNDEFINED, true).map(Fresh::take),
        START => {
            set(vm, state, STATE, Value::from_i32(DONE))?;
            if kind == ZIP {
                close_all(vm, state, Ok(()))?;
            } else if kind != CONCAT {
                let it = get(vm, state, ITER)?;
                close(vm, it)?;
            }
            return iter_result(vm, Value::UNDEFINED, true).map(Fresh::take);
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
    iter_result(vm, Value::UNDEFINED, true).map(Fresh::take)
}

/// `%WrapForValidIteratorPrototype%.next()`: the wrapped iterator's.
fn wrap_next(vm: &mut Vm, this: Value, _: Args<'_>) -> Result<Value> {
    let state = state_of(this, true, "%WrapForValidIteratorPrototype%.next")?;
    let (it, next) = (get(vm, state, ITER)?, get(vm, state, NEXT)?);
    vm.call_b(next, it, &[]).map(Fresh::take)
}

/// `%WrapForValidIteratorPrototype%.return()`: the wrapped iterator's
/// `return()`, if it has one.
fn wrap_return(vm: &mut Vm, this: Value, _: Args<'_>) -> Result<Value> {
    let state = state_of(this, true, "%WrapForValidIteratorPrototype%.return")?;
    let it = get(vm, state, ITER)?;
    let a_return = vm.intern_name("return");
    let sc_m = Scope::new(vm);
    let m = vm.get_atom_b(it, a_return)?.root(&sc_m);
    if m.peek().is_undefined() || m.peek().is_null() {
        return iter_result(vm, Value::UNDEFINED, true).map(Fresh::take);
    }
    vm.call_b(m.peek(), it, &[]).map(Fresh::take)
}

// ---- Iterator ----

/// `Iterator.from(o)`: `o`'s iterator, wrapped unless it inherits from
/// `Iterator.prototype`.
fn from(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    // (A proxy's `getPrototypeOf` may collect: both rooted.)
    let sc = Scope::new(vm);
    let (it, next) = flattenable(vm, arg(&a, 0), true)?.root(&sc);
    let c = object_value((vm.iterator_roots[CONSTRUCTOR]).ptr());
    if vm.instance_of(it.v(), c)? {
        return Ok(it.v());
    }
    new_helper(vm, WRAP, it.v(), next.v(), Value::UNDEFINED, Value::UNDEFINED).map(Fresh::take)
}

/// `Iterator.concat(...items)`: their values in turn (each item an
/// iterable object).
fn concat(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    // take: rooted below; nothing between collects.
    let list = vm.mu().new_array().v();
    {
        let held = Scope::new(vm);
        let _held = held.roots([list]);
        concat_list(vm, list, &a)
    }?;
    new_helper(vm, CONCAT, Value::UNDEFINED, Value::UNDEFINED, list, Value::from_i32(0)).map(Fresh::take)
}

/// `Iterator.concat`'s items' `@@iterator` methods, each with its item,
/// into `list`.
fn concat_list(vm: &mut Vm, list: Value, a: &[Value]) -> Result<()> {
    for &item in a {
        if !is_object(item) {
            return Err(Error::type_error(format!("{} is not an object", crate::vm::shown(item))));
        }
        let sc_m = Scope::new(vm);
        let m = iterator_method(vm, item)?.root(&sc_m);
        if m.peek().is_undefined() {
            return Err(Error::type_error(format!("{} is not iterable", crate::vm::shown(item))));
        }
        crate::vm::array_push(vm, list, &[m.peek(), item])?;
    }
    Ok(())
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
        let e = Error::type_error(format!("{} is not a function", crate::vm::shown(f)));
        return close_throw(vm, it, e);
    }
    Ok(f)
}

fn lazy(vm: &mut Vm, this: Value, a: &[Value], kind: i32, what: &str) -> Result<Value> {
    let it = this_object(this, what)?;
    let f = callable_arg(vm, it, arg(a, 0))?;
    let sc = Scope::new(vm);
    let next = next_method(vm, it)?.root(&sc);
    new_helper(vm, kind, it, next.v(), f, Value::from_i32(0)).map(Fresh::take)
}

fn map(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    lazy(vm, this, &a, MAP, "map")
}

fn filter(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    lazy(vm, this, &a, FILTER, "filter")
}

fn flat_map(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    lazy(vm, this, &a, FLAT_MAP, "flatMap")
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
        return close_throw(vm, it, Error::range_error(format!("{} must be a number", crate::vm::shown(arg(a, 0)))));
    }
    // (Infinity, or a safe integer.)
    let limit = d.trunc();
    if limit < 0.0 || limit.is_finite() && limit > 9007199254740991.0 {
        return close_throw(vm, it, Error::range_error(format!("{d} is out of range")));
    }
    let sc_next = Scope::new(vm);
    let next = next_method(vm, it)?.root(&sc_next);
    new_helper(vm, kind, it, next.peek(), Value::UNDEFINED, Value::number(limit)).map(Fresh::take)
}

fn take(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    limited(vm, this, &a, TAKE, "take")
}

fn drop(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    limited(vm, this, &a, DROP, "drop")
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
    let sc_next = Scope::new(vm);
    let next = next_method(vm, it)?.root(&sc_next);
    // (`next`, and the result found, held across calls: rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([next.peek()]);
    let mut counter = 0.0;
    // take: given at once to `each`, which roots what it keeps.
    while let Some(v) = step_value(vm, it, next.peek())?.take() {
        match each(vm, v, counter) {
            Ok(Some(r)) => {
                {
                    let held = Scope::new(vm);
                    let _held = held.roots([r]);
                    close(vm, it)
                }?;
                return Ok(Some(r));
            }
            Ok(None) => {}
            Err(e) => return close_throw(vm, it, e),
        }
        counter += 1.0;
    }
    Ok(None)
}

fn reduce(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let it = this_object(this, "reduce")?;
    let f = callable_arg(vm, it, arg(&a, 0))?;
    let sc_next = Scope::new(vm);
    let next = next_method(vm, it)?.root(&sc_next);
    // (`next`, and the accumulator in a slot of its own, held across the
    // calls: rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([next.peek(), Value::UNDEFINED]);
    let slot = vm.handles.len() - 1;
    let mut counter = if a.len() >= 2 {
        vm.handles[slot] = a[1];
        0.0
    } else {
        // take: kept in `vm.handles[slot]` at once.
        match step_value(vm, it, next.peek())?.take() {
            Some(v) => vm.handles[slot] = v,
            None => return Err(Error::type_error("Reduce of empty iterator with no initial value")),
        }
        1.0
    };
    // take: given at once to the call, which roots its arguments.
    while let Some(v) = step_value(vm, it, next.peek())?.take() {
        let acc = vm.handles[slot];
        vm.handles[slot] = match vm.call_b(f, Value::UNDEFINED, &[acc, v, Value::number(counter)]) {
            Ok(r) => r.take(),
            Err(e) => return close_throw(vm, it, e),
        };
        counter += 1.0;
    }
    Ok(vm.handles[slot])
}

fn to_array(vm: &mut Vm, this: Value, _: Args<'_>) -> Result<Value> {
    // take: rooted on the next line.
    let out = vm.mu().new_array().v();
    vm.rooted(&[out], |vm| {
        each(vm, this, None, "toArray", |vm, v, _| crate::vm::array_push(vm, out, &[v]).map(|_| None))
    })?;
    Ok(out)
}

fn for_each(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let f = arg(&a, 0);
    each(vm, this, Some(f), "forEach", |vm, v, k| {
        vm.call_b(f, Value::UNDEFINED, &[v, Value::number(k)]).map(|_| None)
    })?;
    Ok(Value::UNDEFINED)
}

/// `some`'s, `every`'s and `find`'s: the first value `f` says `want` of.
fn search(vm: &mut Vm, this: Value, a: &[Value], want: bool, what: &str) -> Result<Option<Value>> {
    let f = arg(a, 0);
    // (Each value, the result if found, rooted across the predicate: its
    // argument slot it may overwrite.)
    let sc = Scope::new(vm);
    let held = sc.root(Value::UNDEFINED);
    each(vm, this, Some(f), what, |vm, v, k| {
        held.set(v);
        let r = vm.call_b(f, Value::UNDEFINED, &[v, Value::number(k)])?;
        Ok((to_boolean(r.peek()) == want).then_some(held.v()))
    })
}

fn some(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    Ok(Value::from_bool(search(vm, this, &a, true, "some")?.is_some()))
}

fn every(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    Ok(Value::from_bool(search(vm, this, &a, false, "every")?.is_none()))
}

fn find(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    Ok(search(vm, this, &a, true, "find")?.unwrap_or(Value::UNDEFINED))
}

/// A copy of array `a` from index `from` on (`CreateArrayFromList`).
#[inline(always)]
fn copy(vm: &mut Vm, a: Value, from: f64) -> Result<Fresh<'_>> {
    copy_raw(vm, a, from).map(Fresh::new)
}

fn copy_raw(vm: &mut Vm, a: Value, from: f64) -> Result<Value> {
    let sc_out = Scope::new(vm);
    let out = vm.mu().new_array().root(&sc_out);
    let len = vm.length_of(a)?;
    let mut i = from;
    while i < len {
        // take: given at once to the next call.
        let v = vm.get_keyed_b(a, Value::number(i))?.take();
        crate::vm::array_push(vm, out.v(), &[v])?;
        i += 1.0;
    }
    Ok(out.v())
}

/// Whether `v` is the string `s`.
fn is_string(v: Value, s: &str) -> bool {
    crate::vm::string_units(v).is_some_and(|u| u.iter().copied().eq(s.encode_utf16()))
}

/// `includes(searchElement, skippedElements)`: whether a value past the
/// first `skippedElements` is `searchElement` (SameValueZero).
fn includes(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let it = this_object(this, "includes")?;
    let (search, skip) = (arg(&a, 0), arg(&a, 1));
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
fn join(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let it = this_object(this, "join")?;
    let sep: Vec<u16> = match arg(&a, 0) {
        s if s.is_undefined() => vec![b',' as u16],
        s => match vm.to_string_b(s) {
            Ok(s) => crate::vm::string_units(s.take()).unwrap_or(&[]).to_vec(),
            Err(e) => return close_throw(vm, it, e),
        },
    };
    let mut out: Vec<u16> = Vec::new();
    let mut first = true;
    each(vm, it, None, "join", |vm, v, _| {
        if !first {
            out.extend_from_slice(&sep);
            // (Separators alone, between `undefined`s, grow it too.)
            crate::vm::check_string_len(out.len())?;
        }
        first = false;
        if !v.is_undefined() && !v.is_null() {
            let s = vm.to_string_b(v)?;
            crate::vm::push_string(&mut out, s.peek())?;
        }
        Ok(None)
    })?;
    vm.try_new_string(&out)
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
        _ => close_throw(vm, it, Error::type_error(format!("{} is not an integral number", crate::vm::shown(v)))),
    }
}

/// `chunks(size)`: arrays of `size` values in turn (the last maybe fewer).
fn chunks(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let it = this_object(this, "chunks")?;
    let size = size_arg(vm, it, arg(&a, 0))?;
    let sc_next = Scope::new(vm);
    let next = next_method(vm, it)?.root(&sc_next);
    let sc_h = Scope::new(vm);
    let h = new_helper(vm, CHUNKS, it, next.peek(), Value::number(size), Value::from_i32(0))?.root(&sc_h);
    let state = Value::from_bits(Obj::of(h.peek()).code() as u64);
    let sc_buffer = Scope::new(vm);
    let buffer = crate::array::new_array_of(vm, &[])?.root(&sc_buffer);
    set(vm, state, INNER, buffer.peek())?;
    set(vm, state, INNER_NEXT, Value::FALSE)?;
    Ok(h.peek())
}

/// `windows(size, undersized)`: arrays of each `size` consecutive values
/// (with "allow-partial", all of them if fewer).
fn windows(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let it = this_object(this, "windows")?;
    let size = size_arg(vm, it, arg(&a, 0))?;
    let undersized = arg(&a, 1);
    let partial = if undersized.is_undefined() || is_string(undersized, "only-full") {
        false
    } else if is_string(undersized, "allow-partial") {
        true
    } else {
        return close_throw(vm, it, Error::type_error("undersized must be \"only-full\" or \"allow-partial\""));
    };
    let sc_next = Scope::new(vm);
    let next = next_method(vm, it)?.root(&sc_next);
    let sc_h = Scope::new(vm);
    let h = new_helper(vm, WINDOWS, it, next.peek(), Value::number(size), Value::from_i32(partial as i32))?.root(&sc_h);
    let state = Value::from_bits(Obj::of(h.peek()).code() as u64);
    let sc_buffer = Scope::new(vm);
    let buffer = crate::array::new_array_of(vm, &[])?.root(&sc_buffer);
    set(vm, state, INNER, buffer.peek())?;
    set(vm, state, INNER_NEXT, Value::FALSE)?;
    Ok(h.peek())
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
    let a_mode = vm.intern_name("mode");
    let m = vm.get_atom_b(options, a_mode)?;
    let mode = if m.peek().is_undefined() || is_string(m.peek(), "shortest") {
        SHORTEST
    } else if is_string(m.peek(), "longest") {
        LONGEST
    } else if is_string(m.peek(), "strict") {
        STRICT
    } else {
        return Err(Error::type_error("mode must be \"shortest\", \"longest\" or \"strict\""));
    };
    let mut padding = Value::UNDEFINED;
    if mode == LONGEST {
        let a_padding = vm.intern_name("padding");
        padding = vm.get_atom_b(options, a_padding)?.take();
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
#[inline(always)]
fn new_zip<'v>(
    vm: &'v mut Vm,
    iters: &[(Value, Value)],
    keys: Value,
    mode: i32,
    padding: &[Value],
) -> Result<Fresh<'v>> {
    new_zip_raw(vm, iters, keys, mode, padding).map(Fresh::new)
}

fn new_zip_raw(vm: &mut Vm, iters: &[(Value, Value)], keys: Value, mode: i32, padding: &[Value]) -> Result<Value> {
    let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
    let nexts: Vec<Value> = iters.iter().map(|p| p.1).collect();
    let sc = Scope::new(vm);
    let its = crate::array::new_array_of(vm, &its)?.root(&sc);
    let nexts = crate::array::new_array_of(vm, &nexts)?.root(&sc);
    let h = new_helper(vm, ZIP, its.v(), nexts.v(), keys, Value::from_i32(mode))?.root(&sc);
    let state = Value::from_bits(Obj::of(h.v()).code() as u64);
    let padding = crate::array::new_array_of(vm, padding)?.root(&sc);
    let open = crate::array::new_array_of(vm, &vec![Value::TRUE; iters.len()])?.root(&sc);
    set(vm, state, INNER, padding.v())?;
    set(vm, state, INNER_NEXT, open.v())?;
    Ok(h.v())
}

/// "longest" mode's padding: `n` values, from iterable `option` (or
/// `undefined`s), or with `keys` its properties of those keys (the
/// iterators closed on an error).
#[inline(always)]
fn zip_padding<'v>(
    vm: &'v mut Vm,
    option: Value,
    keys: Option<&[Key]>,
    iters: &[(Value, Value)],
) -> Result<Fresh<'v, Vec<Value>>> {
    zip_padding_raw(vm, option, keys, iters).map(Fresh::new)
}

fn zip_padding_raw(vm: &mut Vm, option: Value, keys: Option<&[Key]>, iters: &[(Value, Value)]) -> Result<Vec<Value>> {
    let n = iters.len();
    let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
    if option.is_undefined() {
        return Ok(vec![Value::UNDEFINED; n]);
    }
    let mut out = Vec::with_capacity(n);
    if let Some(keys) = keys {
        for &k in keys {
            match vm.get_key_b(option, k) {
                Ok(v) => {
                    let v = v.take();
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
        // take: pushed to vm.handles with `p` after.
        let next = next_method(vm, p)?.take();
        Ok((p, next))
    })();
    let (p, next) = match r {
        Ok(r) => r,
        Err(e) => return close_list(vm, &its, Err(e)).map(|()| Vec::new()),
    };
    // (Held across its steps: rooted, with the values, in the caller's
    // handle scope.)
    vm.handles.extend([p, next]);
    let mut using = true;
    for _ in 0..n {
        if using {
            match step_value(vm, p, next).map(Fresh::transpose) {
                Ok(Some(v)) => {
                    // take: pushed to vm.handles on the next line.
                    let v = v.take();
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
fn zip(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    let iterables = arg(&a, 0);
    if !is_object(iterables) {
        return Err(Error::type_error("Iterator.zip: iterables must be an object"));
    }
    let (mode, padding) = zip_options(vm, arg(&a, 1))?;
    // (`padding`, the input iterator and its `next`, held across calls:
    // rooted.)
    vm.handle_scope(|vm| {
        vm.handles.push(padding);
        let input = crate::symbol::get_iterator(vm, iterables)?;
        vm.handles.push(input);
        // take: pushed to vm.handles on the next line.
        let input_next = next_method(vm, input)?.take();
        vm.handles.push(input_next);
        zip_iterables(vm, input, input_next, mode, padding).map(Fresh::take)
    })
}

#[inline(always)]
fn zip_iterables(vm: &mut Vm, input: Value, input_next: Value, mode: i32, padding: Value) -> Result<Fresh<'_>> {
    zip_iterables_raw(vm, input, input_next, mode, padding).map(Fresh::new)
}

fn zip_iterables_raw(vm: &mut Vm, input: Value, input_next: Value, mode: i32, padding: Value) -> Result<Value> {
    let mut iters: Vec<(Value, Value)> = Vec::new();
    loop {
        let its: Vec<Value> = iters.iter().map(|p| p.0).collect();
        // take: given at once to flattenable, which roots it.
        let v = match step_value(vm, input, input_next).map(Fresh::take) {
            Ok(Some(v)) => v,
            Ok(None) => break,
            Err(e) => return close_list(vm, &its, Err(e)).map(|()| Value::UNDEFINED),
        };
        match flattenable(vm, v, false) {
            Ok(p) => {
                // take: pushed to vm.handles on the next line.
                let p = p.take();
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
    // take: zip_padding pushes its values to vm.handles, in this handle scope.
    let padding = if mode == LONGEST { zip_padding(vm, padding, None, &iters)?.take() } else { Vec::new() };
    new_zip(vm, &iters, Value::UNDEFINED, mode, &padding).map(Fresh::take)
}

/// `Iterator.zipKeyed(iterables, options)`: as `Iterator.zip` over the
/// object `iterables`' enumerable own properties not undefined, its
/// results objects of their keys.
fn zip_keyed(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    let iterables = arg(&a, 0);
    if !is_object(iterables) {
        return Err(Error::type_error("Iterator.zipKeyed: iterables must be an object"));
    }
    let (mode, padding) = zip_options(vm, arg(&a, 1))?;
    vm.handle_scope(|vm| {
        // (Held across calls: rooted.)
        vm.handles.push(padding);
        let all = crate::proxy::own_keys(vm, Obj::of(iterables))?;
        let mut keys = Vec::new();
        let mut iters: Vec<(Value, Value)> = Vec::new();
        for key in all {
            let r = (|| {
                let Some(d) = crate::proxy::get_own_property(vm, Obj::of(iterables), key)? else {
                    return Ok(None);
                };
                if d.enumerable != Some(true) {
                    return Ok(None);
                }
                let sc_v = Scope::new(vm);
                let v = vm.get_key_b(iterables, key)?.root(&sc_v);
                if v.peek().is_undefined() {
                    return Ok(None);
                }
                // take: pushed to vm.handles as the closure returns.
                flattenable(vm, v.peek(), false).map(|p| Some(p.take()))
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
        // take: zip_padding pushes its values to vm.handles, in this handle scope.
        let padding = if mode == LONGEST { zip_padding(vm, padding, Some(&keys), &iters)?.take() } else { Vec::new() };
        let names: Vec<Value> = keys
            .iter()
            .map(|&k| {
                let s = vm.key_value(k);
                if s.is_number() { vm.key_string(k) } else { s }
            })
            .collect();
        vm.handles.extend(names.iter().copied());
        let sc_keys = Scope::new(vm);
        let keys = crate::array::new_array_of(vm, &names)?.root(&sc_keys);
        new_zip(vm, &iters, keys.peek(), mode, &padding).map(Fresh::take)
    })
}

/// A zip's open iterators, closed with completion `c` (IteratorCloseAll).
/// (In a handle scope: each `close` runs user code, and `its` must not
/// move under it. No cell moves while every `return()` runs, as in
/// `zip_step`'s `next()`s.)
fn close_all(vm: &mut Vm, state: Value, c: Result<()>) -> Result<()> {
    vm.handle_scope(|vm| {
        let (iters, open) = (get(vm, state, ITER)?, get(vm, state, INNER_NEXT)?);
        let n = vm.length_of(iters)? as usize;
        let mut its = Vec::new();
        for i in 0..n {
            if vm.get_keyed_b(open, Value::from_i32(i as i32))?.take() == Value::TRUE {
                its.push(vm.get_keyed_b(iters, Value::from_i32(i as i32))?.take());
            }
        }
        vm.handles.extend(its.iter().copied());
        close_list(vm, &its, c)
    })
}

/// A zip's next results (IteratorZip's closure, to its `yield`).
#[inline(always)]
fn zip_step(vm: &mut Vm, state: Value) -> Result<Fresh<'_, Option<Value>>> {
    zip_step_raw(vm, state).map(Fresh::new)
}

fn zip_step_raw(vm: &mut Vm, state: Value) -> Result<Option<Value>> {
    vm.handle_scope(|vm| zip_results(vm, state).map(Fresh::take))
}

#[inline(always)]
fn zip_results(vm: &mut Vm, state: Value) -> Result<Fresh<'_, Option<Value>>> {
    zip_results_raw(vm, state).map(Fresh::new)
}

fn zip_results_raw(vm: &mut Vm, state: Value) -> Result<Option<Value>> {
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
        // raw: an element of the zip's own arrays, which no JS sees.
        let it = vm.get_keyed_b(iters, Value::from_i32(i))?.raw();
        if it.v().is_null() {
            // take: pushed to vm.handles on the next line.
            let v = vm.get_keyed_b(padding, Value::from_i32(i))?.take();
            vm.handles.push(v);
            results.push(v);
            continue;
        }
        // raw: an element of the zip's own arrays, which no JS sees.
        let next = vm.get_keyed_b(nexts, Value::from_i32(i))?.raw();
        match step_value(vm, it.v(), next.v()).map(Fresh::transpose) {
            Ok(Some(v)) => {
                // take: pushed to vm.handles on the next line.
                let v = v.take();
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
                            let (it, next) = (
                                vm.get_keyed_b(iters, Value::from_i32(k))?.take(),
                                vm.get_keyed_b(nexts, Value::from_i32(k))?.take(),
                            );
                            match step_result(vm, it, next).map(Fresh::transpose) {
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
                            any |= vm.get_keyed_b(open, Value::from_i32(k))?.take() == Value::TRUE;
                        }
                        if !any {
                            return Ok(None);
                        }
                        vm.set_keyed(iters, Value::from_i32(i), Value::NULL)?;
                        // take: pushed to vm.handles on the next line.
                        let v = vm.get_keyed_b(padding, Value::from_i32(i))?.take();
                        vm.handles.push(v);
                        results.push(v);
                    }
                }
            }
        }
    }
    if keys.is_undefined() {
        return crate::array::new_array_of(vm, &results).map(|r| Some(r.take()));
    }
    let sc_o = Scope::new(vm);
    let o = vm.mu().new_plain(Value::NULL).root(&sc_o);
    for (i, v) in results.into_iter().enumerate() {
        let sc_k = Scope::new(vm);
        let k = vm.get_keyed_b(keys, Value::from_i32(i as i32))?.root(&sc_k);
        let key = crate::intrinsics::to_property_key(vm, k.peek())?;
        vm.create_data_property(o.v(), key, v)?;
    }
    Ok(Some(o.v()))
}
