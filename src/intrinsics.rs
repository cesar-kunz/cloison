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
    ("throw_const", throw_const),
    ("class_setup", class_setup),
    ("define_method", define_method),
    ("super_get", super_get),
    ("super_set", super_set),
    ("super_call", super_call),
    ("derived_return", derived_return),
    ("class_call_check", class_call_check),
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
    construct_with(vm, f, args, f)
}

/// `new f(…args)` with `new.target` `nt`: a bytecode function's `this`
/// made from `nt.prototype` (a derived class's left to `super()`).
pub(crate) fn construct_with(vm: &mut Vm, f: Value, args: &[Value], nt: Value) -> Result<Value> {
    if f.is_cell() && unsafe { (*as_object(f)).class } == Class::Function {
        let o = as_object(f);
        let code = unsafe { &*(*o).code };
        if !code.constructor() {
            return Err(Error::type_error(format!("{} is not a constructor", crate::vm::display(f))));
        }
        let this = if code.kind == 3 {
            Value::HOLE
        } else if nt == f {
            vm.construct_this(o)
        } else {
            let proto = prototype_of_target(vm, nt)?;
            object_value(vm.new_object(proto))
        };
        if code.uses_new_target {
            vm.new_target = nt;
        }
        let r = call_function(vm, f, this, args);
        vm.new_target = Value::EMPTY;
        let r = r?;
        return Ok(if crate::props::is_object(r) { r } else { this });
    }
    if !is_constructor(f) {
        return Err(Error::type_error(format!("{} is not a constructor", crate::vm::display(f))));
    }
    let r = vm.construct_other(f, args)?;
    // (A builtin subclassed: its object gets the subclass's prototype.)
    if nt != f && crate::props::is_object(r) {
        let proto = prototype_of_target(vm, nt)?;
        vm.set_prototype_of(as_object(r), proto);
    }
    Ok(r)
}

/// `nt.prototype` if an object, else `Object.prototype`.
fn prototype_of_target(vm: &mut Vm, nt: Value) -> Result<Value> {
    let a = vm.a_prototype;
    let p = vm.get(nt, a)?;
    Ok(if crate::props::is_object(p) { p } else { object_value(vm.object_prototype) })
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

/// An assignment to a `const`: `[name]`.
fn throw_const(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let n = String::from_utf16_lossy(crate::vm::string_units(a[0]).unwrap_or(&[]));
    Err(Error::type_error(format!("Assignment to constant variable '{n}'.")))
}

/// A class being made: `[heritage, F]` (heritage `HOLE` if none): sets
/// up `F.prototype` and `F`'s prototype; returns `F.prototype`.
fn class_setup(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (h, f) = (a[0], a[1]);
    let function_proto = object_value(vm.function_prototype);
    let (proto_parent, ctor_parent) = if h == Value::HOLE {
        (object_value(vm.object_prototype), function_proto)
    } else if h.is_null() {
        (Value::NULL, function_proto)
    } else if !is_constructor(h) {
        return Err(Error::type_error(format!(
            "Class extends value {} is not a constructor or null",
            crate::vm::display(h)
        )));
    } else {
        let a_prototype = vm.a_prototype;
        let p = vm.get(h, a_prototype)?;
        if !(p.is_null() || crate::props::is_object(p)) {
            return Err(Error::type_error(format!(
                "Class extends value does not have valid prototype property {}",
                crate::vm::display(p)
            )));
        }
        (p, h)
    };
    let proto = object_value(vm.new_object(proto_parent));
    let a_constructor = vm.intern("constructor");
    let d = Desc::data(f, true, false, true);
    vm.define_own_property(as_object(proto), Key::Name(a_constructor), &d)?;
    let a_prototype = vm.a_prototype;
    let d = Desc::data(proto, false, false, false);
    vm.define_own_property(as_object(f), Key::Name(a_prototype), &d)?;
    vm.set_prototype_of(as_object(f), ctor_parent);
    Ok(proto)
}

/// A class's method: `[o, key, f, computed]`, not enumerable (a computed
/// key's function named after it).
fn define_method(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[1])?;
    if a[3] == Value::TRUE {
        set_function_name(vm, a[2], key, "")?;
    }
    let d = Desc::data(a[2], true, false, true);
    vm.define_own_property(as_object(a[0]), key, &d)?;
    Ok(a[0])
}

/// Name function `f` after property key `key`, `prefix` first.
fn set_function_name(vm: &mut Vm, f: Value, key: Key, prefix: &str) -> Result<()> {
    let k = vm.key_value(key);
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    if !prefix.is_empty() {
        units.push(b' ' as u16);
    }
    units.extend_from_slice(crate::vm::string_units(to_string(vm, k)?).unwrap_or(&[]));
    let name = vm.new_string(&units);
    let a_name = vm.a_name;
    let d = Desc::data(name, false, false, true);
    vm.define_own_property(as_object(f), Key::Name(a_name), &d)?;
    Ok(())
}

/// `o[key]` from `start` on, accessors called with `receiver`.
fn get_from(vm: &mut Vm, start: Value, key: Key, receiver: Value) -> Result<Value> {
    let mut o = start;
    while crate::props::is_object(o) {
        if let Some(d) = vm.own_property(as_object(o), key) {
            if d.get.is_some() || d.set.is_some() {
                return match d.get {
                    Some(g) if !g.is_undefined() => call_function(vm, g, receiver, &[]),
                    _ => Ok(Value::UNDEFINED),
                };
            }
            return Ok(d.value.unwrap_or(Value::UNDEFINED));
        }
        o = unsafe { (*(*as_object(o)).shape).proto };
    }
    if o.is_cell() {
        // (A string's prototype chain.)
        return vm.get_key(o, key);
    }
    Ok(Value::UNDEFINED)
}

/// The prototype of home object `home` (where `super` looks).
fn super_base(home: Value) -> Result<Value> {
    if !crate::props::is_object(home) {
        return Err(Error::type_error("'super' keyword unexpected here"));
    }
    Ok(unsafe { (*(*as_object(home)).shape).proto })
}

/// `super[key]`: `[home, key, this]`.
fn super_get(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let base = super_base(a[0])?;
    let key = to_property_key(vm, a[1])?;
    if base.is_null() {
        return Err(Error::type_error("Cannot read properties of null (reading a super property)"));
    }
    get_from(vm, base, key, a[2])
}

/// `super[key] = v`: `[home, key, v, this]`.
fn super_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let base = super_base(a[0])?;
    let key = to_property_key(vm, a[1])?;
    let (v, this) = (a[2], a[3]);
    let mut o = base;
    while crate::props::is_object(o) {
        if let Some(d) = vm.own_property(as_object(o), key) {
            if d.get.is_some() || d.set.is_some() {
                return match d.set {
                    Some(s) if !s.is_undefined() => call_function(vm, s, this, &[v]).map(|_| v),
                    _ => Err(Error::type_error("Cannot set a property that has only a getter")),
                };
            }
            if d.writable == Some(false) {
                return Err(Error::type_error("Cannot assign to a read-only property"));
            }
            break;
        }
        o = unsafe { (*(*as_object(o)).shape).proto };
    }
    if !crate::props::is_object(this) {
        return Err(Error::type_error("Cannot assign to a property of a primitive"));
    }
    match vm.own_property(as_object(this), key) {
        Some(d) if d.get.is_some() || d.set.is_some() || d.writable == Some(false) => {
            return Err(Error::type_error("Cannot assign to a read-only property"));
        }
        Some(_) => {
            let d = Desc {
                value: Some(v),
                ..Default::default()
            };
            vm.define_own_property(as_object(this), key, &d)?;
        }
        None => {
            if !vm.create_data_property(this, key, v).is_ok() {
                return Err(Error::type_error("Cannot add a property: the object is not extensible"));
            }
        }
    }
    Ok(v)
}

/// `super(…args)`: `[F, new.target, args, this]`, `F` the class whose
/// prototype is the constructor called.
fn super_call(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (f, nt, this) = (a[0], a[1], a[3]);
    if !crate::props::is_object(f) {
        return Err(Error::type_error("'super' keyword unexpected here"));
    }
    let parent = unsafe { (*(*as_object(f)).shape).proto };
    if !is_constructor(parent) {
        return Err(Error::type_error(format!(
            "Super constructor {} of anonymous class is not a constructor",
            crate::vm::display(parent)
        )));
    }
    let args = elements(vm, a[2])?;
    let r = construct_with(vm, parent, &args, nt)?;
    if this != Value::HOLE {
        return Err(Error::reference_error("Super constructor may only be called once"));
    }
    Ok(r)
}

/// What a derived class's constructor returns, `[v, this]`: `v` if an
/// object, else `this` (initialized: `super()` called).
fn derived_return(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let (v, this) = (a[0], a[1]);
    if crate::props::is_object(v) {
        return Ok(v);
    }
    if !v.is_undefined() {
        return Err(Error::type_error("Derived constructors may only return object or undefined"));
    }
    if this == Value::HOLE {
        return Err(Error::reference_error(
            "Must call super constructor in derived class before accessing 'this' or returning from derived constructor",
        ));
    }
    Ok(this)
}

/// A class constructor called without `new`: `[new.target]`.
fn class_call_check(_: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[0].is_undefined() {
        return Err(Error::type_error("Class constructor cannot be invoked without 'new'"));
    }
    Ok(a[0])
}

#[allow(dead_code)]
fn object(o: *mut Object) -> Value {
    object_value(o)
}
