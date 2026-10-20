//! `Reflect`: the object operations as functions.

use crate::intrinsics::{construct_with, get_from, is_constructor, ordinary_set, to_property_key};
use crate::interp::call_function;
use crate::object::{DONT_ENUM, Object, as_object, object_value};
use crate::props::is_object;
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm};

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// The target argument, which must be an object.
fn target(a: &[Value], what: &str) -> Result<*mut Object> {
    let t = arg(a, 0);
    if !is_object(t) {
        return Err(Error::type_error(format!("Reflect.{what} called on non-object")));
    }
    Ok(as_object(t))
}

/// An arguments list (CreateListFromArrayLike), which must be an object.
fn list(vm: &mut Vm, v: Value) -> Result<Vec<Value>> {
    if !is_object(v) {
        return Err(Error::type_error("CreateListFromArrayLike called on non-object"));
    }
    crate::array::apply_list(vm, v)
}

pub(crate) fn install(vm: &mut Vm) {
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    vm.define_natives(
        r,
        &[
            ("apply", reflect_apply as NativeFn, 3),
            ("construct", reflect_construct, 2),
            ("defineProperty", reflect_define_property, 3),
            ("deleteProperty", reflect_delete_property, 2),
            ("get", reflect_get, 2),
            ("getOwnPropertyDescriptor", reflect_get_own_property_descriptor, 2),
            ("getPrototypeOf", reflect_get_prototype_of, 1),
            ("has", reflect_has, 2),
            ("isExtensible", reflect_is_extensible, 1),
            ("ownKeys", reflect_own_keys, 1),
            ("preventExtensions", reflect_prevent_extensions, 1),
            ("set", reflect_set, 3),
            ("setPrototypeOf", reflect_set_prototype_of, 2),
        ],
    );
    let g = vm.global;
    let a = vm.intern("Reflect");
    vm.add_property_with(g, a, object_value(r), DONT_ENUM);
}

fn reflect_apply(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let f = arg(a, 0);
    if !crate::props::is_callable(f) {
        return Err(Error::type_error("Reflect.apply target is not a function"));
    }
    vm.handle_scope(|vm| {
        let args = list(vm, arg(a, 2))?;
        call_function(vm, f, arg(a, 1), &args)
    })
}

fn reflect_construct(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let f = arg(a, 0);
    if !is_constructor(f) {
        return Err(Error::type_error("Reflect.construct target is not a constructor"));
    }
    let nt = if a.len() > 2 { a[2] } else { f };
    if !is_constructor(nt) {
        return Err(Error::type_error("Reflect.construct newTarget is not a constructor"));
    }
    vm.handle_scope(|vm| {
        let args = list(vm, arg(a, 1))?;
        construct_with(vm, f, &args, nt)
    })
}

pub(crate) fn reflect_define_property(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "defineProperty")?;
    let key = to_property_key(vm, arg(a, 1))?;
    let d = vm.to_descriptor(arg(a, 2))?;
    crate::proxy::define_property(vm, o, key, &d).map(Value::from_bool)
}

pub(crate) fn reflect_delete_property(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "deleteProperty")?;
    let key = to_property_key(vm, arg(a, 1))?;
    crate::proxy::delete(vm, o, key).map(Value::from_bool)
}

pub(crate) fn reflect_get(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "get")?;
    let key = to_property_key(vm, arg(a, 1))?;
    let receiver = if a.len() > 2 { a[2] } else { object_value(o) };
    if crate::proxy::chain_has_proxy(vm, o) {
        return crate::proxy::generic_get(vm, object_value(o), key, receiver);
    }
    get_from(vm, object_value(o), key, receiver)
}

pub(crate) fn reflect_get_own_property_descriptor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "getOwnPropertyDescriptor")?;
    let key = to_property_key(vm, arg(a, 1))?;
    Ok(match crate::proxy::get_own_property(vm, o, key)? {
        Some(d) => vm.from_descriptor(&d),
        None => Value::UNDEFINED,
    })
}

fn reflect_get_prototype_of(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "getPrototypeOf")?;
    crate::proxy::get_prototype_of(vm, o)
}

pub(crate) fn reflect_has(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "has")?;
    let key = to_property_key(vm, arg(a, 1))?;
    vm.has_key(object_value(o), key).map(Value::from_bool)
}

fn reflect_is_extensible(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "isExtensible")?;
    crate::proxy::is_extensible(vm, o).map(Value::from_bool)
}

/// `Reflect.ownKeys(o)`: its string keys, then its symbols.
pub(crate) fn reflect_own_keys(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "ownKeys")?;
    let keys = crate::proxy::own_keys(vm, o)?;
    let out = vm.new_array();
    vm.handle_scope(|vm| {
        vm.handles.push(out);
        for key in keys {
            let s = vm.key_value(key);
            let s = if s.is_number() { vm.key_string(key) } else { s };
            crate::vm::array_push(vm, out, &[s])?;
        }
        Ok(out)
    })
}

fn reflect_prevent_extensions(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "preventExtensions")?;
    crate::proxy::prevent_extensions(vm, o).map(Value::from_bool)
}

fn reflect_set(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "set")?;
    let key = to_property_key(vm, arg(a, 1))?;
    let receiver = if a.len() > 3 { a[3] } else { object_value(o) };
    let v = arg(a, 2);
    if crate::proxy::chain_has_proxy(vm, o) {
        return crate::proxy::generic_set(vm, object_value(o), key, v, receiver).map(Value::from_bool);
    }
    ordinary_set(vm, object_value(o), key, v, receiver).map(Value::from_bool)
}

fn reflect_set_prototype_of(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = target(a, "setPrototypeOf")?;
    let proto = arg(a, 1);
    if !(proto.is_null() || is_object(proto)) {
        return Err(Error::type_error("Object prototype may only be an Object or null"));
    }
    crate::proxy::set_prototype_of(vm, o, proto).map(Value::from_bool)
}
