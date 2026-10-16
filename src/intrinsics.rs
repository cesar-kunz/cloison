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
    ("property_key", property_key),
    ("literal_proto", literal_proto),
    ("throw_invalid_target", throw_invalid_target),
    ("class_setup", class_setup),
    ("define_method", define_method),
    ("super_get", super_get),
    ("super_set", super_set),
    ("super_set_sloppy", super_set_sloppy),
    ("super_call", super_call),
    ("derived_return", derived_return),
    ("class_call_check", class_call_check),
    ("get_iterator", get_iterator),
    ("iterator_step", iterator_step),
    ("iterator_close", iterator_close),
    ("iterator_close_quiet", iterator_close_quiet),
    ("pattern_step", pattern_step),
    ("iterator_rest", iterator_rest),
    ("require_coercible", require_coercible),
    ("object_rest", object_rest),
    ("direct_eval", direct_eval),
    ("eval_env", eval_env),
    ("null_proto", null_proto),
    ("to_object", to_object),
    ("with_set_strict", with_set_strict),
    ("generator_mode", generator_mode),
    ("delegate_step", delegate_step),
    ("delegate_done", delegate_done),
    ("throw_delete", throw_delete),
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
    if crate::symbol::is_symbol(v) {
        return Ok(Key::Name(crate::symbol::symbol_atom(v)));
    }
    let v = if v.is_cell() && crate::vm::string_units(v).is_none() {
        let p = crate::interp::to_primitive_string(v);
        if vm.conv_thrown {
            return Err(vm.take_conv_error());
        }
        if crate::symbol::is_symbol(p) {
            return Ok(Key::Name(crate::symbol::symbol_atom(p)));
        }
        to_string(vm, p)?
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
    if !vm.define_own_property(as_object(a[0]), key, &d)? {
        return Err(Error::type_error("Cannot redefine a non-configurable property"));
    }
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
/// ToPropertyKey of `[k]`: a string or a symbol.
fn property_key(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[0])?;
    Ok(vm.key_value(key))
}

/// An object literal's `__proto__: v`, `[o, v]`: `o`'s prototype `v` if an
/// object or null (else nothing).
fn literal_proto(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (o, v) = (a[0], a[1]);
    if crate::props::is_object(v) || v.is_null() {
        vm.set_prototype_of(as_object(o), v);
    }
    Ok(o)
}

/// A call assigned to (see `Gen::invalid_target`).
fn throw_invalid_target(_: &mut Vm, _: &[Value]) -> Result<Value> {
    Err(Error::reference_error("Invalid left-hand side in assignment"))
}

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
    if !vm.define_own_property(as_object(a[0]), key, &d)? {
        return Err(Error::type_error("Cannot redefine a non-configurable property"));
    }
    Ok(a[0])
}

/// Name function `f` after property key `key`, `prefix` first.
fn set_function_name(vm: &mut Vm, f: Value, key: Key, prefix: &str) -> Result<()> {
    let k = vm.key_value(key);
    let mut units: Vec<u16> = prefix.encode_utf16().collect();
    if !prefix.is_empty() {
        units.push(b' ' as u16);
    }
    if crate::symbol::is_symbol(k) {
        // `[description]`.
        let d = Value::from_bits(unsafe { (*as_object(k)).code } as u64);
        if let Some(d) = crate::vm::string_units(d) {
            units.push(b'[' as u16);
            units.extend_from_slice(d);
            units.push(b']' as u16);
        }
    } else {
        units.extend_from_slice(crate::vm::string_units(to_string(vm, k)?).unwrap_or(&[]));
    }
    let name = vm.new_string(&units);
    let a_name = vm.a_name;
    let d = Desc::data(name, false, false, true);
    vm.define_own_property(as_object(f), Key::Name(a_name), &d)?;
    Ok(())
}

/// `o[key]` from `start` on, accessors called with `receiver`.
pub(crate) fn get_from(vm: &mut Vm, start: Value, key: Key, receiver: Value) -> Result<Value> {
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
    if !ordinary_set(vm, base, key, v, this)? {
        return Err(Error::type_error("Cannot assign to a read-only property"));
    }
    Ok(v)
}

/// `super[key] = v` in sloppy code: failing silently.
fn super_set_sloppy(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let base = super_base(a[0])?;
    let key = to_property_key(vm, a[1])?;
    ordinary_set(vm, base, key, a[2], a[3])?;
    Ok(a[2])
}

/// The spec's OrdinarySet: `key` looked up from `start`, stored as
/// `receiver`'s (a setter called on it); false where it can't be.
pub(crate) fn ordinary_set(vm: &mut Vm, start: Value, key: Key, v: Value, receiver: Value) -> Result<bool> {
    let mut o = start;
    while crate::props::is_object(o) {
        if let Some(d) = vm.own_property(as_object(o), key) {
            if d.get.is_some() || d.set.is_some() {
                return match d.set {
                    Some(s) if !s.is_undefined() => call_function(vm, s, receiver, &[v]).map(|_| true),
                    _ => Ok(false),
                };
            }
            if d.writable == Some(false) {
                return Ok(false);
            }
            break;
        }
        o = unsafe { (*(*as_object(o)).shape).proto };
    }
    if !crate::props::is_object(receiver) {
        return Ok(false);
    }
    match vm.own_property(as_object(receiver), key) {
        Some(d) if d.get.is_some() || d.set.is_some() || d.writable == Some(false) => Ok(false),
        Some(_) => {
            let d = Desc {
                value: Some(v),
                ..Default::default()
            };
            vm.define_own_property(as_object(receiver), key, &d)
        }
        None => Ok(vm.create_data_property(receiver, key, v).is_ok()),
    }
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

/// `for … of`'s iterator: `[iterable]`.
fn get_iterator(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    crate::symbol::get_iterator(vm, a[0])
}

/// `[iterator]`: its next value, or `HOLE` when done.
fn iterator_step(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    Ok(crate::symbol::step(vm, a[0])?.unwrap_or(Value::HOLE))
}

/// A `for … of` left early: `[iterator]` closed.
fn iterator_close(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    crate::symbol::close(vm, a[0]).map(|()| Value::UNDEFINED)
}

/// A `for … of` left by an exception: `[iterator]` closed, its own errors
/// ignored (the exception goes on).
fn iterator_close_quiet(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let _ = crate::symbol::close(vm, a[0]);
    Ok(Value::UNDEFINED)
}

/// An array pattern's step: `[iterator]` (`undefined` once done): its
/// next value, or `HOLE`.
fn pattern_step(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[0].is_undefined() {
        return Ok(Value::HOLE);
    }
    iterator_step(vm, a)
}

/// An array pattern's rest: `[iterator]`'s remaining values.
fn iterator_rest(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let out = vm.new_array();
    if a[0].is_undefined() {
        return Ok(out);
    }
    while let Some(v) = crate::symbol::step(vm, a[0])? {
        crate::vm::array_push(vm, out, &[v])?;
    }
    Ok(out)
}

/// An object pattern's source: `[v]`, not `null` or `undefined`.
fn require_coercible(_: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[0].is_undefined() || a[0].is_null() {
        return Err(Error::type_error(format!("Cannot destructure '{}' as it is {}.", crate::vm::display(a[0]), crate::vm::display(a[0]))));
    }
    Ok(a[0])
}

/// An object pattern's rest: `[source, keys]`, a new object with
/// `source`'s own enumerable properties but those keys.
fn object_rest(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let src = vm.to_object(a[0])?;
    let excluded: Vec<Key> = {
        let ks = elements(vm, a[1])?;
        let mut out = Vec::new();
        for k in ks {
            out.push(to_property_key(vm, k)?);
        }
        out
    };
    let out = object_value(vm.new_object(object_value(vm.object_prototype)));
    for (key, enumerable) in vm.own_keys(src) {
        if !enumerable || excluded.contains(&key) {
            continue;
        }
        let v = vm.get_key(object_value(src), key)?;
        vm.create_data_property(out, key, v)?;
    }
    Ok(out)
}

/// A direct `eval(source)`: `[eval, source, scope, this, strict]`. Not
/// the builtin `eval`: an ordinary call. Else `source` (a string) is
/// compiled with the caller's scope object `scope` around it (see
/// `js::compile_direct_eval`), its `var`s and functions (sloppy code's)
/// made there first, and run with the caller's `this`.
fn direct_eval(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (f, src, env, this, strict) = (a[0], a[1], a[2], a[3], a[4] == Value::TRUE);
    let builtin = f.is_cell() && as_object(f) == vm.eval_function;
    if !builtin {
        return call_function(vm, f, Value::UNDEFINED, &[src]);
    }
    let Some(u) = crate::vm::string_units(src) else {
        return Ok(src);
    };
    let text = String::from_utf16_lossy(u);
    let module = crate::js::compile_direct_eval(&text, strict).map_err(|e| {
        if e.contains("unsupported") {
            Error(format!("unsupported: in eval'd code: {e}"), None)
        } else {
            Error(format!("SyntaxError: {e}"), None)
        }
    })?;
    // Strict code's `var`s are its own: in a scope object of its own.
    let own_strict = module.functions.get(module.script).is_some_and(|f| f.strict);
    // (In parameters' defaults, a `var arguments` is an error.)
    if a.get(5) == Some(&Value::TRUE) && !own_strict && module.declared.iter().any(|n| n == "arguments") {
        return Err(Error("SyntaxError: 'arguments' declared by eval in a parameter expression".into(), None));
    }
    let local = object_value(vm.new_object(Value::NULL));
    let target = if own_strict || !crate::props::is_object(env) { local } else { env };
    for n in &module.declared {
        let atom = vm.intern(n);
        if vm.own_property(as_object(target), Key::Name(atom)).is_none() {
            vm.create_data_property(target, Key::Name(atom), Value::UNDEFINED)?;
        }
    }
    // (Its `var`s are the scope objects', not globals.)
    let mut module = module;
    module.declared.clear();
    let code = vm.load(module);
    let env = if crate::props::is_object(env) { env } else { local };
    vm.eval_envs.push([env, local]);
    let r = crate::interp::run_script_with(vm, code, this);
    vm.eval_envs.pop();
    r
}

/// Direct `eval`'s code's first instruction's: the caller's scope object.
fn eval_env(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let k = a[0].as_number().unwrap_or(0.0) as usize;
    Ok(vm.eval_envs.last().map_or(Value::UNDEFINED, |e| e[k.min(1)]))
}

/// `[o]` without a prototype.
fn null_proto(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    vm.set_prototype_of(as_object(a[0]), Value::NULL);
    Ok(a[0])
}

/// ToObject: `[v]` (a `with` statement's object).
fn to_object(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    Ok(object_value(vm.to_object(a[0])?))
}

/// Strict code's store to a `with` object's property: `[o, name, v]`,
/// a ReferenceError if `o` no longer has it.
fn with_set_strict(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[1])?;
    if !vm.has_key(a[0], key)? {
        return Err(Error::reference_error(format!("{} is not defined", crate::vm::display(a[1]))));
    }
    vm.put_key(a[0], key, a[2])?;
    Ok(a[2])
}

/// How generator `[g]` was resumed (`generator::NEXT`…).
fn generator_mode(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    Ok(crate::generator::mode(vm, a[0]))
}

/// `yield*`'s step: `[iterator, received, generator]`, the inner
/// iterator's `next`, `throw` or `return` called as the generator was
/// resumed: its result object.
fn delegate_step(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (it, received) = (a[0], a[1]);
    let mode = crate::generator::mode(vm, a[2]).as_number().unwrap_or(0.0) as i32;
    let name = match mode {
        crate::generator::THROW => "throw",
        crate::generator::RETURN => "return",
        _ => "next",
    };
    let atom = vm.intern(name);
    let m = vm.get(it, atom)?;
    if m.is_undefined() || m.is_null() {
        match mode {
            crate::generator::RETURN => return crate::symbol::iter_result(vm, received, true),
            crate::generator::THROW => {
                let _ = crate::symbol::close(vm, it);
                return Err(Error::type_error("The iterator does not provide a 'throw' method"));
            }
            _ => {}
        }
    }
    let r = call_function(vm, m, it, &[received])?;
    if !crate::props::is_object(r) {
        return Err(Error::type_error(format!("Iterator result {} is not an object", crate::vm::display(r))));
    }
    Ok(r)
}

/// Whether iterator result `[r]` is done.
fn delegate_done(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let a_done = vm.intern("done");
    let d = vm.get(a[0], a_done)?;
    Ok(Value::from_bool(crate::interp::to_boolean(d)))
}

/// Strict code's failed `delete`.
fn throw_delete(_: &mut Vm, _: &[Value]) -> Result<Value> {
    Err(Error::type_error("Cannot delete a non-configurable property"))
}

#[allow(dead_code)]
fn object(o: *mut Object) -> Value {
    object_value(o)
}
