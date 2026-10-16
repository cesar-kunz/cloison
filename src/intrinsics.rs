//! Runtime helpers that bytecode calls through `Intrinsic(id, first, n)`:
//! `acc = INTRINSICS[id](r[first..first + n])`. What the front end
//! desugars newer syntax into (spread, templates, computed keys, …), slow
//! paths all: the JIT leaves them to the interpreter.

use crate::interp::{call_function, to_string_units};
use crate::object::{Class, Object, as_object, object_value};
use crate::props::Desc;
use crate::value::Value;
use crate::vm::{Error, Key, Result, Vm};

pub(crate) type Intrinsic = fn(&mut Vm, &[Value]) -> Result<Value>;

/// The intrinsics, by name: the index is the id.
pub(crate) const INTRINSICS: &[(&str, Intrinsic)] = &[
    ("hole", hole),
    ("spread", spread),
    ("call_spread", call_spread),
    ("construct_spread", construct_spread),
    ("template", template),
    ("template_object", template_object),
    ("define_field", define_field),
    ("define_getter", define_getter),
    ("define_setter", define_setter),
];

/// The id of the intrinsic `name`.
pub fn id(name: &str) -> u8 {
    INTRINSICS
        .iter()
        .position(|(n, _)| *n == name)
        .unwrap_or_else(|| panic!("no intrinsic {name}")) as u8
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// ToString, its errors thrown.
pub(crate) fn to_string(vm: &mut Vm, v: Value) -> Result<Value> {
    if crate::vm::string_units(v).is_some() {
        return Ok(v);
    }
    let units = to_string_units(v);
    if vm.conv_thrown {
        return Err(vm.take_conv_error());
    }
    Ok(vm.new_string(&units))
}

/// ToPropertyKey, its errors thrown.
pub(crate) fn to_property_key(vm: &mut Vm, v: Value) -> Result<Key> {
    let v = if v.is_cell() && crate::vm::string_units(v).is_none() {
        to_string(vm, v)?
    } else {
        v
    };
    Ok(vm.property_key(v))
}

/// `[…, , …]`: the array one longer, with a hole.
fn hole(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let arr = a[0];
    let n = unsafe { (*as_object(arr)).elems_len };
    let a_length = vm.a_length;
    vm.put_key(arr, Key::Name(a_length), Value::number(n as f64 + 1.0))?;
    Ok(arr)
}

/// `…x` into array `a[0]`: each value `a[1]` iterates over appended.
fn spread(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let arr = a[0];
    crate::collections::iterate(vm, a[1], |vm, v| {
        crate::vm::array_push(vm, arr, &[v]).map(|_| ())
    })?;
    Ok(arr)
}

/// The elements of array `arr` (made by the front end: dense, its own).
fn elements(vm: &mut Vm, arr: Value) -> Result<Vec<Value>> {
    let n = unsafe { (*as_object(arr)).elems_len };
    (0..n).map(|i| vm.get_keyed(arr, Value::from_i32(i as i32))).collect()
}

/// `f(…args)` with `this`: `[f, this, args]`.
fn call_spread(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let args = elements(vm, a[2])?;
    call_function(vm, a[0], a[1], &args)
}

/// `new f(…args)`: `[f, args]`.
fn construct_spread(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let args = elements(vm, a[1])?;
    construct(vm, a[0], &args)
}

/// Whether `f` can be `new`ed.
pub(crate) fn is_constructor(f: Value) -> bool {
    if !f.is_cell() {
        return false;
    }
    let o = as_object(f);
    unsafe {
        match (*o).class {
            Class::Function => (*(*o).code).constructor(),
            Class::Native => (*(*o).code).constructs,
            _ => false,
        }
    }
}

/// `new f(…args)`, from the runtime.
pub(crate) fn construct(vm: &mut Vm, f: Value, args: &[Value]) -> Result<Value> {
    if f.is_cell() && unsafe { (*as_object(f)).class } == Class::Function {
        let o = as_object(f);
        if !unsafe { (*(*o).code).constructor() } {
            return Err(Error::type_error(format!("{} is not a constructor", crate::vm::display(f))));
        }
        let this = vm.construct_this(o);
        let r = call_function(vm, f, this, args)?;
        return Ok(if r.is_cell() && crate::props::is_object(r) { r } else { this });
    }
    if !is_constructor(f) {
        return Err(Error::type_error(format!("{} is not a constructor", crate::vm::display(f))));
    }
    vm.construct_other(f, args)
}

/// A template literal: `[s0, e1, s1, e2, …]`, each substitution's
/// ToString between the strings.
fn template(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mut units: Vec<u16> = Vec::new();
    for (i, &v) in a.iter().enumerate() {
        if i % 2 == 0 {
            units.extend_from_slice(crate::vm::string_units(v).unwrap_or(&[]));
        } else {
            if !v.is_cell() || crate::vm::string_units(v).is_some() {
                if v.is_cell() || v.is_number() || v.is_bool() || v.is_undefined() || v.is_null() {
                } else {
                    // (A symbol.)
                    return Err(Error::type_error("Cannot convert a Symbol value to a string"));
                }
            }
            let s = to_string(vm, v)?;
            units.extend_from_slice(crate::vm::string_units(s).unwrap_or(&[]));
        }
    }
    Ok(vm.new_string(&units))
}

/// A tagged template's strings array: `[site, cooked0, raw0, …]`, one
/// array per site (`site` a number unique to it), frozen, with its frozen
/// `raw` array.
fn template_object(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let site = a[0].as_number().unwrap_or(0.0) as u64;
    if let Some(&o) = vm.template_cache.get(&site) {
        return Ok(object_value(o));
    }
    let strings = vm.new_array();
    let raw = vm.new_array();
    for pair in a[1..].chunks(2) {
        crate::vm::array_push(vm, strings, &[pair[0]])?;
        crate::vm::array_push(vm, raw, &[pair[1]])?;
    }
    vm.seal(as_object(raw), true);
    let a_raw = vm.intern("raw");
    let d = Desc {
        value: Some(raw),
        writable: Some(false),
        enumerable: Some(false),
        configurable: Some(false),
        ..Default::default()
    };
    vm.define_own_property(as_object(strings), Key::Name(a_raw), &d)?;
    vm.seal(as_object(strings), true);
    vm.template_cache.insert(site, as_object(strings));
    Ok(strings)
}

/// `{[k]: v}`: `[o, k, v]`.
fn define_field(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[1])?;
    vm.create_data_property(a[0], key, a[2])?;
    Ok(a[0])
}

fn define_accessor(vm: &mut Vm, a: &[Value], set: bool) -> Result<Value> {
    let key = to_property_key(vm, a[1])?;
    let f = arg(a, 2);
    let d = Desc {
        get: (!set).then_some(f),
        set: set.then_some(f),
        enumerable: Some(arg(a, 3).is_undefined()),
        configurable: Some(true),
        ..Default::default()
    };
    vm.define_own_property(as_object(a[0]), key, &d)?;
    Ok(a[0])
}

/// `{get [k]() {…}}`: `[o, k, f]` (a fourth argument: not enumerable,
/// a class's).
fn define_getter(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    define_accessor(vm, a, false)
}

fn define_setter(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    define_accessor(vm, a, true)
}

#[allow(dead_code)]
fn object(o: *mut Object) -> Value {
    object_value(o)
}
