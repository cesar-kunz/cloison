//! Runtime helpers that bytecode calls through `Intrinsic(id, first, n)`:
//! `acc = INTRINSICS[id](r[first..first + n])`. What the front end
//! desugars newer syntax into (spread, templates, computed keys, …), slow
//! paths all: the JIT leaves them to the interpreter.

use crate::interp::{call_function, to_string_units};
use crate::js::ast::{EVAL_BLOCK_SCOPED, EVAL_BOXED, EVAL_CATCH, EVAL_CONST};
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
    ("template_part", template_part),
    ("template_object", template_object),
    ("define_field", define_field),
    ("define_named_field", define_named_field),
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
    ("strict_eval", strict_eval),
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
    ("direct_eval_bound", direct_eval_bound),
    ("eval_env", eval_env),
    ("null_object", null_object),
    ("to_object", to_object),
    ("with_set_strict", with_set_strict),
    ("with_has", with_has),
    ("scope_delete", scope_delete),
    ("map_arguments_to_scope", map_arguments_to_scope),
    ("scope_var", scope_var),
    ("async_from_sync", |vm, a| crate::promise::async_from_sync(vm, a[0])),
    ("generator_mode", generator_mode),
    ("delegate_step", delegate_step),
    ("delegate_done", delegate_done),
    ("throw_delete", throw_delete),
    ("for_in_has", for_in_has),
    ("exp", exp),
    ("copy_data_properties", copy_data_properties),
    ("private_name", private_name),
    ("private_get", private_get),
    ("private_set", private_set),
    ("private_define", private_define),
    ("private_method", private_method),
    ("private_has", private_has),
    ("async_gen_yield", async_gen_yield),
    ("async_iterator_kind", async_iterator_kind),
    ("get_async_iterator", get_async_iterator),
    ("async_iterator_next", async_iterator_next),
    ("async_iterator_value", async_iterator_value),
    ("async_iterator_close", async_iterator_close),
    ("async_delegate_step", async_delegate_step),
    ("eval_scope", eval_scope),
    ("eval_scope_get", eval_scope_get),
    ("eval_scope_flush", eval_scope_flush),
    ("bigint", crate::bigint::literal),
    ("dispose_new", |vm, _| crate::dispose::new_state(vm, false)),
    ("dispose_add", dispose_add),
    ("dispose_run", dispose_run),
    ("dispose_run_async", dispose_run_async),
    ("dynamic_import", |vm, a| crate::module::dynamic_import(vm, a[0], a[1], a[2], a[3])),
    ("import_meta", |vm, a| crate::module::import_meta(vm, a[0])),
];

/// `using x = v` (`[state, v, async]`): `v` added to the statement list's
/// resources, and the binding's value.
fn dispose_add(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    crate::dispose::add_resource(vm, a[0], a[1], a[2] == Value::TRUE)?;
    Ok(a[1])
}

/// A statement list with `using` declarations ended (`[state, threw,
/// error]`): its resources disposed of, what that throws (or `error`,
/// else) thrown.
fn dispose_run(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let completion = if a[1] == Value::TRUE { Err(Error::thrown(a[2])) } else { Ok(Value::UNDEFINED) };
    crate::dispose::dispose_resources(vm, a[0], completion)
}

/// As [`dispose_run`] with `await using` declarations: a promise of their
/// disposal to await, or `undefined` if there is nothing to dispose of (no
/// await then; `error` thrown, if any).
fn dispose_run_async(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if crate::dispose::is_empty(a[0]) {
        return if a[1] == Value::TRUE { Err(Error::thrown(a[2])) } else { Ok(Value::UNDEFINED) };
    }
    crate::dispose::dispose_resources_async(vm, a[0], a[1] == Value::TRUE, a[2])
}

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
        return Ok(Key::Name(crate::symbol::symbol_atom(vm, v)));
    }
    let v = if v.is_cell() && crate::vm::string_units(v).is_none() {
        let p = crate::interp::to_primitive_string(v);
        if vm.conv_thrown {
            return Err(vm.take_conv_error());
        }
        if crate::symbol::is_symbol(p) {
            return Ok(Key::Name(crate::symbol::symbol_atom(vm, p)));
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
    let n = crate::far::array_length(as_object(arr));
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
    // (A proxy's trampoline takes `new.target`.)
    if vm.proxies_exist && crate::proxy::is_proxy(vm, as_object(f)) {
        vm.new_target = nt;
    }
    let derived = std::mem::replace(&mut vm.derived_construct, if nt != f { f } else { Value::UNDEFINED });
    let outer = std::mem::replace(&mut vm.native_new_target, if nt != f { (f, nt) } else { (Value::EMPTY, Value::EMPTY) });
    let r = vm.construct_other(f, args);
    let r = if vm.conv_failed() { Err(vm.take_conv_error()) } else { r };
    vm.derived_construct = derived;
    // (Unless the constructor read the target's prototype itself.)
    let taken = nt != f && vm.native_new_target.0 != f;
    vm.native_new_target = outer;
    let r = r?;
    // (A builtin subclassed: its object gets the subclass's prototype, or
    // keeps the builtin's if that isn't an object.)
    if nt != f && !taken && crate::props::is_object(r) {
        let a = vm.a_prototype;
        let proto = vm.get(nt, a)?;
        if crate::props::is_object(proto) {
            vm.set_prototype_of(as_object(r), proto);
        } else if !vm.realms.is_empty() {
            // (The builtin's prototype of `nt`'s realm.)
            let own = unsafe { (*(*as_object(r)).shape).proto };
            let p = crate::realm::default_proto(vm, nt, own)?;
            if p != own {
                vm.set_prototype_of(as_object(r), p);
            }
        }
        // (A view's buffer checked again after: the spec reads the
        // prototype first, and its getter may have detached or shrunk it.)
        let o = as_object(r);
        let class = unsafe { (*o).class };
        // (A subclass's `RegExp` has no legacy features: see
        // `builtins::note_legacy`.)
        if class == Class::RegExp {
            unsafe { (*o).flags |= crate::object::LEGACY_DISABLED };
        }
        if matches!(class, Class::TypedArray | Class::DataView) {
            if crate::typed::is_detached(crate::typed::buffer_of(o)) {
                return Err(Error::type_error("Cannot construct a view on a detached ArrayBuffer"));
            }
            let fits = if class == Class::TypedArray {
                crate::typed::ta_length(o).is_some()
            } else {
                crate::typed::view_byte_length(o).is_some()
            };
            if !fits {
                return Err(Error::range_error("The view is out of its buffer's bounds"));
            }
        }
    }
    Ok(r)
}

/// `nt.prototype` if an object, else `Object.prototype`.
fn prototype_of_target(vm: &mut Vm, nt: Value) -> Result<Value> {
    let a = vm.a_prototype;
    let p = vm.get(nt, a)?;
    if crate::props::is_object(p) {
        return Ok(p);
    }
    let op = object_value(vm.object_prototype);
    crate::realm::default_proto(vm, nt, op)
}

/// A template literal: `[s0, e1, s1, e2, …]`, each substitution's
/// ToString between the strings.
fn template(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mut units: Vec<u16> = Vec::new();
    for (i, &v) in a.iter().enumerate() {
        if i % 2 == 0 {
            units.extend_from_slice(crate::vm::string_units(v).unwrap_or(&[]));
        } else {
            let s = template_string(vm, v)?;
            units.extend_from_slice(crate::vm::string_units(s).unwrap_or(&[]));
        }
    }
    Ok(vm.new_string(&units))
}

/// A template literal's substitution `[e]` as its ToString, before the
/// next one is evaluated (as the literal's evaluation orders them).
fn template_part(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    template_string(vm, a[0])
}

fn template_string(vm: &mut Vm, v: Value) -> Result<Value> {
    if !v.is_cell() || crate::vm::string_units(v).is_some() {
        if v.is_cell() || v.is_number() || v.is_bool() || v.is_undefined() || v.is_null() {
        } else {
            // (A symbol.)
            return Err(Error::type_error("Cannot convert a Symbol value to a string"));
        }
    }
    to_string(vm, v)
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

/// `[o, k, f]` of `{ [k]: function () {} }`: as `define_field`, the
/// anonymous function or class `f` named after the key first (but a
/// class's own static `name`).
fn define_named_field(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[1])?;
    let a_name = vm.a_name;
    let own = vm.own_property(as_object(a[2]), Key::Name(a_name));
    if own.is_none_or(|d| d.value.is_some_and(|v| crate::vm::string_units(v).is_some_and(|u| u.is_empty()))) {
        set_function_name(vm, a[2], key, "")?;
    }
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
    // (Through a proxy on the chain: its trap.)
    if crate::props::is_object(base) {
        return crate::proxy::generic_get(vm, base, key, a[2]);
    }
    get_from(vm, base, key, a[2])
}

/// OrdinarySet from `base` (a proxy on its chain: the proxy's [[Set]]).
fn super_store(vm: &mut Vm, base: Value, key: Key, v: Value, this: Value) -> Result<bool> {
    if crate::props::is_object(base) {
        return crate::proxy::generic_set(vm, base, key, v, this);
    }
    ordinary_set(vm, base, key, v, this)
}

/// `super[key] = v`: `[home, key, v, this]`.
fn super_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let base = super_base(a[0])?;
    let key = to_property_key(vm, a[1])?;
    let (v, this) = (a[2], a[3]);
    if !super_store(vm, base, key, v, this)? {
        return Err(Error::type_error("Cannot assign to a read-only property"));
    }
    Ok(v)
}

/// `super[key] = v` in sloppy code: failing silently.
fn super_set_sloppy(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let base = super_base(a[0])?;
    let key = to_property_key(vm, a[1])?;
    super_store(vm, base, key, a[2], a[3])?;
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
    // (A proxy receiver's traps: `getOwnPropertyDescriptor`,
    // `defineProperty`.)
    let r = as_object(receiver);
    match crate::proxy::get_own_property(vm, r, key)? {
        Some(d) if d.get.is_some() || d.set.is_some() || d.writable == Some(false) => Ok(false),
        Some(_) => {
            let d = Desc {
                value: Some(v),
                ..Default::default()
            };
            crate::proxy::define_property(vm, r, key, &d)
        }
        None => crate::proxy::define_property(vm, r, key, &Desc::data(v, true, true, true)),
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

pub(crate) const DERIVED_RESULT: &str = "TypeError: Derived constructors may only return object or undefined";
pub(crate) const DERIVED_THIS: &str =
    "ReferenceError: Must call super constructor in derived class before accessing 'this' or returning from derived constructor";

/// What a derived class's constructor returns, `[v, this]`: `v` if an
/// object, else `this` (initialized: `super()` called).
fn derived_return(_: &mut Vm, a: &[Value]) -> Result<Value> {
    let (v, this) = (a[0], a[1]);
    if crate::props::is_object(v) {
        return Ok(v);
    }
    if !v.is_undefined() {
        return Err(Error(DERIVED_RESULT.into(), None));
    }
    if this == Value::HOLE {
        return Err(Error(DERIVED_THIS.into(), None));
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
    let it = crate::symbol::get_iterator(vm, a[0])?;
    crate::symbol::record(vm, it)
}

/// `[record]` (`get_iterator`'s): its next value, or `HOLE` when done.
fn iterator_step(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (it, next) = crate::symbol::record_parts(a[0]);
    Ok(crate::symbol::step_with(vm, it, next)?.unwrap_or(Value::HOLE))
}

/// A record's iterator (a record's or a value's: `undefined` once an
/// array pattern's is done).
fn iterator_of(r: Value) -> Value {
    if crate::props::is_object(r) { crate::symbol::record_parts(r).0 } else { r }
}

/// A `for … of` left early: `[record]`'s iterator closed.
fn iterator_close(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    crate::symbol::close(vm, iterator_of(a[0])).map(|()| Value::UNDEFINED)
}

/// A `for … in`'s key `[object, key]` still to visit: the object (if not
/// a primitive, whose keys stay) still has it, not deleted in the loop.
fn for_in_has(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    // (Nor through a proxy's traps.)
    if !crate::props::is_object(a[0]) || crate::proxy::chain_has_proxy(vm, as_object(a[0])) {
        return Ok(Value::TRUE);
    }
    vm.has_property(a[0], a[1]).map(Value::from_bool)
}

/// A `for … of` left by an exception: `[iterator]` closed, its own errors
/// ignored (the exception goes on).
fn iterator_close_quiet(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let _ = crate::symbol::close(vm, iterator_of(a[0]));
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
    let (it, next) = crate::symbol::record_parts(a[0]);
    while let Some(v) = crate::symbol::step_with(vm, it, next)? {
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
    for key in crate::proxy::own_keys(vm, src)? {
        if excluded.contains(&key) || !crate::proxy::get_own_property(vm, src, key)?.is_some_and(|d| d.enumerable == Some(true)) {
            continue;
        }
        let v = vm.get_key(object_value(src), key)?;
        vm.create_data_property(out, key, v)?;
    }
    Ok(out)
}

/// A direct `eval(source)`: `[eval, source, scope, this, strict, in
/// parameters, scope for its declarations, with objects…]`. Not
/// the builtin `eval`: an ordinary call. Else `source` (a string) is
/// compiled with the caller's scope object `scope` around it (see
/// `js::compile_direct_eval`), its `var`s and functions (sloppy code's)
/// made there first, and run with the caller's `this`. The code is kept
/// to run again for the same source in the same kind of place (see
/// `Vm::eval_cache`): only its declarations are made at each run.
fn direct_eval(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (f, src, env, this, strict) = (a[0], a[1], a[2], a[3], a[4] == Value::TRUE);
    let builtin = f.is_cell() && as_object(f) == vm.eval_function;
    if !builtin {
        return call_function(vm, f, Value::UNDEFINED, &[src]);
    }
    let Some(u) = crate::vm::string_units(src) else {
        return Ok(src);
    };
    // (The objects of the `with` statements around it, outermost first.)
    let withs = a.get(7..).unwrap_or(&[]);
    let place = crate::vm::EvalPlace::Direct { strict, withs: withs.len() };
    let hash = Vm::eval_hash(u, place);
    // (`kept`: its caches stay when it has run.)
    let (code, own_strict, declared, kept) = match vm.cached_eval(u, place, hash) {
        Some(e) => (e.code, e.own_strict, e.declared.clone(), true),
        None => {
            let source: Box<[u16]> = u.into();
            let text = crate::vm::source_text(u);
            let mut module = crate::js::compile_direct_eval(&text, strict, withs.len(), &[]).map_err(eval_error)?;
            // Strict code's `var`s are its own: in a scope object of its own.
            let own_strict = module.functions.get(module.script).is_some_and(|f| f.strict);
            let declared: std::rc::Rc<[crate::object::Atom]> =
                std::mem::take(&mut module.declared).iter().map(|n| vm.intern(n)).collect();
            // (Its `var`s are the scope objects', not globals.)
            let code = vm.load(module);
            let e = crate::vm::EvalCode {
                source,
                place,
                code,
                own_strict,
                declared: declared.clone(),
                globals: None,
                used: 0,
                recent: true,
            };
            let kept = vm.keep_eval(hash, e);
            (code, own_strict, declared, kept)
        }
    };
    // (In parameters' defaults, a `var arguments` is an error.)
    if a.get(5) == Some(&Value::TRUE) && !own_strict && declared.iter().any(|&n| vm.atom_name(n) == "arguments") {
        return Err(Error("SyntaxError: 'arguments' declared by eval in a parameter expression".into(), None));
    }
    let local = object_value(vm.new_object(Value::NULL));
    // (Declared in the function's scope object, under the one of the
    // other bindings it sees: see `Gen::call`.)
    let decl = a.get(6).copied().filter(|&d| crate::props::is_object(d)).unwrap_or(env);
    let target = if own_strict || !crate::props::is_object(decl) { local } else { decl };
    let over = target == decl && env != decl && crate::props::is_object(env);
    // A `var` by a block-scoped binding's name (but a catch parameter's)
    // is an error.
    for &n in declared.iter().filter(|_| over) {
        let kind = binding_kind(vm, env, Key::Name(n)).unwrap_or(0);
        if kind & EVAL_BLOCK_SCOPED as i32 != 0 && kind & EVAL_CATCH as i32 == 0 {
            return Err(Error(format!("SyntaxError: Identifier '{}' has already been declared", vm.atom_name(n)), None));
        }
    }
    for &atom in declared.iter() {
        if vm.own_property(as_object(target), Key::Name(atom)).is_none() {
            vm.create_data_property(target, Key::Name(atom), Value::UNDEFINED)?;
        }
        // A `var` of the function's shadows an enclosing function's
        // binding: the scope object over the function's has it no more
        // (and it takes back its value unchanged).
        if over && binding_kind(vm, env, Key::Name(atom)).unwrap_or(0) & EVAL_BLOCK_SCOPED as i32 == 0 {
            vm.force_delete_named(as_object(env), atom);
        }
    }
    let env = if crate::props::is_object(env) { env } else { local };
    // (Its names looked up in the caller's scope object, then the
    // function's: see `eval_scope`.)
    let fenv = if crate::props::is_object(decl) { decl } else { env };
    vm.push_eval_envs([env, local, fenv].into_iter().chain(withs.iter().copied()));
    let r = crate::interp::run_script_with(vm, code, this, kept, std::ptr::null_mut());
    vm.pop_eval_envs();
    r
}

/// A direct `eval(source)` whose code sees the bindings around it as its
/// own upvalues: `direct_eval`'s arguments, then the site's bindings
/// function, its description (see `Gen::call`: the block-scoped count,
/// the function's upvalue count, then each binding's kind, `EVAL_CONST`…,
/// upvalue and name) and its number. As V8 compiles an `eval`'s code for
/// the scopes around it (`DeserializeScopeChain`): no scope object of
/// accessors for them at each call (`eval_scope`), the code runs with the
/// bindings function as its frame's, and is kept by the site (see
/// `Vm::eval_cache`). An `eval` in its code would not see them so: that
/// code gets the scope object.
fn direct_eval_bound(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (carrier, site, number) = (a[a.len() - 3], a[a.len() - 2], a[a.len() - 1]);
    let a = &a[..a.len() - 3];
    let (f, src, env, this, strict) = (a[0], a[1], a[2], a[3], a[4] == Value::TRUE);
    if !(f.is_cell() && as_object(f) == vm.eval_function) {
        return call_function(vm, f, Value::UNDEFINED, &[src]);
    }
    let Some(u) = crate::vm::string_units(src) else {
        return Ok(src);
    };
    let withs = a.get(7..).unwrap_or(&[]);
    if u.windows(4).any(|w| w == [b'e' as u16, b'v' as u16, b'a' as u16, b'l' as u16]) {
        let site = site_bindings(site);
        let bindings: Vec<_> = site.2.iter().map(|(n, k, up)| (n.as_str(), *k, *up)).collect();
        return eval_through_scope(vm, a, carrier, site.0, &bindings);
    }
    let params = a.get(5) == Some(&Value::TRUE);
    let site_number = number.as_number().unwrap_or(0.0) as u64;
    let place = crate::vm::EvalPlace::Bound { strict, withs: withs.len(), site: site_number, params };
    let hash = Vm::eval_hash(u, place);
    let (code, own_strict, declared, kept) = match vm.cached_eval(u, place, hash) {
        Some(e) => (e.code, e.own_strict, e.declared.clone(), true),
        None => {
            let source: Box<[u16]> = u.into();
            let text = crate::vm::source_text(u);
            let (_, nups, bindings) = site_bindings(site);
            let bound: Vec<(crate::js::ast::Name, u8)> = (0..nups)
                .map(|k| match bindings.iter().find(|b| b.2 == k) {
                    Some((name, kind, _)) => (name.as_str().into(), *kind as u8),
                    None => ("%none".into(), 0),
                })
                .collect();
            let mut module = crate::js::compile_direct_eval(&text, strict, withs.len(), &bound).map_err(eval_error)?;
            let own_strict = module.functions.get(module.script).is_some_and(|f| f.strict);
            let declared: std::rc::Rc<[crate::object::Atom]> =
                std::mem::take(&mut module.declared).iter().map(|n| vm.intern(n)).collect();
            // (In parameters' defaults, a `var arguments` is an error; a
            // `var` by a block-scoped binding's name, but a catch
            // parameter's, is.)
            for &n in declared.iter().filter(|_| !own_strict) {
                let name = vm.atom_name(n);
                if params && name == "arguments" {
                    return Err(Error("SyntaxError: 'arguments' declared by eval in a parameter expression".into(), None));
                }
                if bindings.iter().any(|(b, kind, _)| {
                    b == name && kind & EVAL_BLOCK_SCOPED as i32 != 0 && kind & EVAL_CATCH as i32 == 0
                }) {
                    return Err(Error(format!("SyntaxError: Identifier '{name}' has already been declared"), None));
                }
            }
            let code = vm.load(module);
            let e = crate::vm::EvalCode {
                source,
                place,
                code,
                own_strict,
                declared: declared.clone(),
                globals: None,
                used: 0,
                recent: true,
            };
            let kept = vm.keep_eval(hash, e);
            (code, own_strict, declared, kept)
        }
    };
    // (Strict code's own scope object: sloppy code's has none, see
    // `parse::parse_pass`.)
    let decl = a.get(6).copied().filter(|&d| crate::props::is_object(d)).unwrap_or(env);
    let local = if own_strict || !crate::props::is_object(decl) {
        object_value(vm.new_object(Value::NULL))
    } else {
        Value::UNDEFINED
    };
    let target = if own_strict || !crate::props::is_object(decl) { local } else { decl };
    for &atom in declared.iter() {
        if vm.own_property(as_object(target), Key::Name(atom)).is_none() {
            vm.create_data_property(target, Key::Name(atom), Value::UNDEFINED)?;
        }
    }
    let fenv = if crate::props::is_object(decl) { decl } else { local };
    vm.push_eval_envs([Value::UNDEFINED, local, fenv].into_iter().chain(withs.iter().copied()));
    let r = crate::interp::run_script_with(vm, code, this, kept, as_object(carrier));
    vm.pop_eval_envs();
    r
}

/// A direct `eval` site's description (see [`direct_eval_bound`]): its
/// block-scoped count, its bindings function's upvalue count, and each
/// binding's name, kind and upvalue.
fn site_bindings(site: Value) -> (usize, usize, Vec<(String, i32, usize)>) {
    let text = crate::vm::source_text(crate::vm::string_units(site).unwrap_or(&[]));
    let mut lines = text.lines();
    let mut counts = lines.next().unwrap_or("").split(' ').map(|n| n.parse::<usize>().unwrap_or(0));
    let (nlex, nups) = (counts.next().unwrap_or(0), counts.next().unwrap_or(0));
    let bindings = lines
        .filter_map(|l| {
            let mut w = l.splitn(3, ' ');
            let (kind, up) = (w.next()?.parse().ok()?, w.next()?.parse().ok()?);
            Some((w.next()?.to_string(), kind, up))
        })
        .collect();
    (nlex, nups, bindings)
}

/// [`direct_eval_bound`]'s `eval` through a scope object of its bindings'
/// accessors (`a` [`direct_eval`]'s arguments), as `Gen::call` makes
/// otherwise: `eval_scope`, then `direct_eval`, then `eval_scope_flush`.
fn eval_through_scope(vm: &mut Vm, a: &[Value], carrier: Value, nlex: usize, bindings: &[(&str, i32, usize)]) -> Result<Value> {
    let mut sa = vec![a[2], Value::from_i32(nlex as i32), carrier];
    let mut names = Vec::with_capacity(bindings.len());
    for &(name, kind, up) in bindings {
        let n = crate::builtins::str_value(vm, name);
        names.push(n);
        sa.extend([n, n, Value::from_i32(((up as i32) << 4) | kind)]);
    }
    let scope = eval_scope(vm, &sa)?;
    let mut da = a.to_vec();
    da[2] = scope;
    let r = direct_eval(vm, &da)?;
    let decl = a.get(6).copied().unwrap_or(Value::UNDEFINED);
    let mut fa = vec![scope, decl, Value::from_i32(nlex as i32)];
    fa.extend(names);
    eval_scope_flush(vm, &fa)?;
    Ok(r)
}

/// A direct `eval(source)` of a strict script's own: `[eval, source]`.
/// As the builtin `eval`'s (indirect) of strict code.
fn strict_eval(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (f, src) = (a[0], a[1]);
    if !(f.is_cell() && as_object(f) == vm.eval_function) {
        return call_function(vm, f, Value::UNDEFINED, &[src]);
    }
    let Some(u) = crate::vm::string_units(src) else {
        return Ok(src);
    };
    crate::builtins::global_eval(vm, u, true)
}

/// An `eval`'s compile error, thrown.
pub(crate) fn eval_error(e: String) -> Error {
    if e.contains("unsupported") {
        Error(format!("unsupported: in eval'd code: {e}"), None)
    } else {
        Error(format!("SyntaxError: {e}"), None)
    }
}

/// Direct `eval`'s code's first instructions': `[k]` the caller's scope
/// object (0), the eval's own (1), a `with` object (2…).
fn eval_env(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let k = a[0].as_number().unwrap_or(0.0) as usize;
    let envs = vm.eval_depth.checked_sub(1).and_then(|d| vm.eval_envs.get(d));
    Ok(envs.and_then(|e| e.get(k).copied()).unwrap_or(Value::UNDEFINED))
}

/// A plain object without a prototype, with room for `[capacity]`
/// properties. (Made so, not given a null prototype afterwards: changing
/// a prototype drops every function's compiled code.)
fn null_object(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let capacity = a[0].as_number().unwrap_or(0.0) as u32;
    Ok(object_value(vm.new_object_with(crate::object::Class::Plain, Value::NULL, capacity)))
}

/// ToObject: `[v]` (a `with` statement's object).
fn to_object(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    Ok(object_value(vm.to_object(a[0])?))
}

/// `[scope, name, value]`: a binding of a function's scope object, its
/// own (a parameter, `var` or function), not deletable. (Added so, not
/// made non-configurable afterwards: changing a property's attributes
/// drops every function's compiled code.)
fn scope_var(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let name = vm.property_key(a[1]);
    let d = Desc::data(a[2], true, true, false);
    vm.define_own_property(as_object(a[0]), name, &d)?;
    Ok(a[0])
}

/// `[name, o]`: whether a `with` over `o` binds `name` (HasBinding of an
/// object environment): `o` has it, and `o[Symbol.unscopables]` doesn't
/// say so.
fn with_has(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (k, o) = (a[0], a[1]);
    if !vm.has_property(o, k)? {
        return Ok(Value::FALSE);
    }
    let au = vm.intern(&format!("{}Symbol.unscopables", crate::symbol::SYMBOL_MARK));
    let u = vm.get(o, au)?;
    if crate::props::is_object(u) {
        let blocked = vm.get_keyed(u, k)?;
        return Ok(Value::from_bool(!crate::interp::to_boolean(blocked)));
    }
    Ok(Value::TRUE)
}

/// A mapped `arguments` object of a function with a direct `eval`
/// (`[arguments, scope, parameter names…]`): its elements mapped to the
/// scope object's properties of those names, where the function keeps its
/// parameters (see `Gen::function`), not to the registers.
fn map_arguments_to_scope(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (o, env) = (as_object(a[0]), as_object(a[1]));
    let mut names = Vec::new();
    for &n in &a[2..] {
        match to_property_key(vm, n)? {
            Key::Name(atom) => names.push(atom),
            Key::Index(_) => return Ok(Value::UNDEFINED),
        }
    }
    if let Some(m) = vm.mapped_args.iter_mut().rev().find(|m| m.o == o) {
        m.scope = (env, names);
    }
    Ok(Value::UNDEFINED)
}

/// `delete name` of a binding a scope object has (`[name, o]`, see
/// `Gen::with_lookup`): deleted from the object on `o`'s chain that owns
/// it, as from its environment. A direct eval's scope object inherits
/// its function's variables, which can't be deleted.
fn scope_delete(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[0])?;
    let mut o = a[1];
    while crate::props::is_object(o) {
        if vm.own_property(as_object(o), key).is_some() {
            return Ok(Value::from_bool(vm.delete_property(o, a[0])?));
        }
        o = unsafe { (*(*as_object(o)).shape).proto };
    }
    Ok(Value::TRUE)
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

/// How generator `[g]` was resumed (`generator::NEXT`…); a mode given
/// for `g` is itself (`yield*`'s before its first resumption).
fn generator_mode(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[0].is_number() {
        return Ok(a[0]);
    }
    Ok(crate::generator::mode(vm, a[0]))
}

/// `yield*`'s step: `[iterator, received, mode]`, the inner iterator's
/// `next`, `throw` or `return` called as the generator was resumed at the
/// `yield*`: `mode` is `next` for the first step, then the generator,
/// whose mode is read only here (no call per step to fetch it).
fn delegate_step(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let mode = generator_mode(vm, &a[2..])?;
    delegate_call(vm, a[0], a[1], mode.as_number().unwrap_or(0.0) as i32)
}

/// An async generator's `yield*` step: `[iterator, received, mode]`, the
/// mode the generator last resumed in at the `yield`.
fn async_delegate_step(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    delegate_call(vm, a[0], a[1], a[2].as_number().unwrap_or(0.0) as i32)
}

/// `iterator.next(received)` (or with `mode`, `throw` or `return`): the
/// result.
/// (`record`'s `next`, read once; `throw` and `return` as they are then.)
fn delegate_call(vm: &mut Vm, record: Value, received: Value, mode: i32) -> Result<Value> {
    let (it, next) = crate::symbol::record_parts(record);
    let m = match mode {
        crate::generator::THROW | crate::generator::RETURN => {
            let atom = vm.intern(if mode == crate::generator::THROW { "throw" } else { "return" });
            vm.get(it, atom)?
        }
        _ => next,
    };
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

/// `a ** b` (Number::exponentiate, as `Math.pow`).
fn exp(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let x = crate::interp::to_numeric(arg(a, 0));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let y = crate::interp::to_numeric(arg(a, 1));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    if crate::bigint::is_bigint(x) || crate::bigint::is_bigint(y) {
        let r = crate::bigint::binary(crate::bigint::BinOp::Exp, x, y);
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
        return Ok(r);
    }
    Ok(Value::number(crate::math::pow(x.to_number(), y.to_number())))
}

/// `{...source}` into object `[target, source]`: the source's own
/// enumerable properties, symbols' too (none for null or undefined).
fn copy_data_properties(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (target, source) = (a[0], a[1]);
    if source == Value::UNDEFINED || source == Value::NULL {
        return Ok(target);
    }
    let src = vm.to_object(source)?;
    let keys = crate::proxy::own_keys(vm, src)?;
    for key in keys {
        // (Enumerable when reached: a getter may change that.)
        if !crate::proxy::get_own_property(vm, src, key)?.is_some_and(|d| d.enumerable == Some(true)) {
            continue;
        }
        let v = vm.get_key(object_value(src), key)?;
        vm.create_data_property(target, key, v)?;
    }
    Ok(target)
}

/// A class's private name `[#x]`: a new private symbol.
fn private_name(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    Ok(crate::symbol::new_private_symbol(vm, a[0]))
}

/// The own property of `o` private symbol `s` keys, if `o` has one.
fn private_property(vm: &mut Vm, o: Value, s: Value) -> Option<Desc> {
    if !crate::props::is_object(o) {
        return None;
    }
    let key = Key::Name(crate::symbol::symbol_atom(vm, s));
    vm.own_property(as_object(o), key)
}

/// `o.#x`: `[o, symbol]`.
fn private_get(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (o, s) = (a[0], a[1]);
    match private_property(vm, o, s) {
        Some(Desc { get: Some(g), .. }) => {
            if g.is_undefined() {
                return Err(Error::type_error(format!(
                    "'{}' was defined without a getter",
                    crate::symbol::description_string(s)
                )));
            }
            call_function(vm, g, o, &[])
        }
        Some(Desc { get: None, set: Some(_), .. }) => Err(Error::type_error(format!(
            "'{}' was defined without a getter",
            crate::symbol::description_string(s)
        ))),
        Some(d) => Ok(d.value.unwrap_or(Value::UNDEFINED)),
        None => Err(Error::type_error(format!(
            "Cannot read private member {} from an object whose class did not declare it",
            crate::symbol::description_string(s)
        ))),
    }
}

/// `o.#x = v`: `[o, symbol, v]`, `v`.
fn private_set(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (o, s, v) = (a[0], a[1], a[2]);
    let key = Key::Name(crate::symbol::symbol_atom(vm, s));
    match private_property(vm, o, s) {
        Some(d) if d.get.is_some() || d.set.is_some() => {
            match d.set.filter(|f| !f.is_undefined()) {
                Some(f) => call_function(vm, f, o, &[v])?,
                None => {
                    return Err(Error::type_error(format!(
                        "'{}' was defined without a setter",
                        crate::symbol::description_string(s)
                    )));
                }
            };
        }
        Some(d) if d.writable == Some(false) => {
            return Err(Error::type_error(format!(
                "Private method '{}' is not writable",
                crate::symbol::description_string(s)
            )));
        }
        Some(_) => {
            let d = Desc { value: Some(v), ..Desc::default() };
            vm.define_own_property(as_object(o), key, &d)?;
        }
        None => {
            return Err(Error::type_error(format!(
                "Cannot write private member {} to an object whose class did not declare it",
                crate::symbol::description_string(s)
            )));
        }
    }
    Ok(v)
}

/// Define private property `key` of `o` as `d`, unless `o` has it.
fn private_add(vm: &mut Vm, o: Value, s: Value, d: &Desc) -> Result<()> {
    if private_property(vm, o, s).is_some() {
        return Err(Error::type_error(format!(
            "Cannot initialize {} twice on the same object",
            crate::symbol::description_string(s)
        )));
    }
    let key = Key::Name(crate::symbol::symbol_atom(vm, s));
    if !vm.define_own_property(as_object(o), key, d)? {
        return Err(Error::type_error(format!(
            "Cannot define private member {} on a non-extensible object",
            crate::symbol::description_string(s)
        )));
    }
    Ok(())
}

/// A private field `#x = v` of a new instance: `[o, symbol, v]`.
fn private_define(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    private_add(vm, a[0], a[1], &Desc::data(a[2], true, false, true))?;
    Ok(a[2])
}

/// A private method (`kind` 0), getter (1) or setter (2) of an instance
/// or (a static one's) a class: `[o, symbol, f, kind]`. A getter and a
/// setter of the name make one accessor.
fn private_method(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (o, s, f) = (a[0], a[1], a[2]);
    let kind = a[3].to_number() as u8;
    if kind == 0 {
        private_add(vm, o, s, &Desc::data(f, false, false, true))?;
        return Ok(o);
    }
    let (mut get, mut set) = (Value::UNDEFINED, Value::UNDEFINED);
    if let Some(d) = private_property(vm, o, s) {
        match (d.get, d.set) {
            (Some(g), Some(st)) if (kind == 1 && g.is_undefined()) || (kind == 2 && st.is_undefined()) => {
                (get, set) = (g, st);
                let key = Key::Name(crate::symbol::symbol_atom(vm, s));
                vm.delete_key(o, key)?;
            }
            _ => {
                return Err(Error::type_error(format!(
                    "Cannot initialize {} twice on the same object",
                    crate::symbol::description_string(s)
                )));
            }
        }
    }
    if kind == 1 {
        get = f;
    } else {
        set = f;
    }
    private_add(vm, o, s, &Desc::accessor(get, set, false, true))?;
    Ok(o)
}

/// `#x in o`: `[o, symbol]`.
fn private_has(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (o, s) = (a[0], a[1]);
    if !crate::props::is_object(o) {
        return Err(Error::type_error(format!(
            "Cannot use 'in' operator to search for '{}' in {}",
            crate::symbol::description_string(s),
            crate::vm::display(o)
        )));
    }
    Ok(Value::from_bool(private_property(vm, o, s).is_some()))
}

/// An async generator's `yield`: `[generator]` marked as yielding the
/// value it suspends with.
fn async_gen_yield(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    crate::generator::async_gen_yield(vm, a[0]);
    Ok(Value::UNDEFINED)
}

/// `[v]`'s `Symbol.asyncIterator` method, if it has one.
fn async_iterator_method(vm: &mut Vm, v: Value) -> Result<Option<Value>> {
    let a = vm.intern(&format!("{}Symbol.asyncIterator", crate::symbol::SYMBOL_MARK));
    let o = object_value(vm.to_object(v)?);
    let m = vm.get(o, a)?;
    Ok((!m.is_undefined() && !m.is_null()).then_some(m))
}

/// A `for await`'s iterable `[v]`: whether it has no async iterator, its
/// sync one used instead.
fn async_iterator_kind(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(async_iterator_method(vm, a[0])?.is_none()))
}

/// `[v, sync]`: `v`'s async iterator (with `sync`, its iterator).
fn get_async_iterator(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    if a[1] == Value::TRUE {
        let it = crate::symbol::get_iterator(vm, a[0])?;
        return crate::symbol::record(vm, it);
    }
    let m = async_iterator_method(vm, a[0])?.unwrap_or(Value::UNDEFINED);
    if !crate::props::is_callable(m) {
        return Err(Error::type_error("Symbol.asyncIterator is not a function"));
    }
    let it = call_function(vm, m, a[0], &[])?;
    if !crate::props::is_object(it) {
        return Err(Error::type_error("Result of the Symbol.asyncIterator method is not an object"));
    }
    crate::symbol::record(vm, it)
}

/// `[record]`: `iterator.next()`, by the `next` the record read.
fn async_iterator_next(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (it, next) = crate::symbol::record_parts(a[0]);
    if !crate::props::is_callable(next) {
        return Err(Error::type_error("iterator.next is not a function"));
    }
    call_function(vm, next, it, &[])
}

/// A `for await`'s iterator result `[r]`: its value, or `HOLE` if done.
fn async_iterator_value(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let r = a[0];
    if !crate::props::is_object(r) {
        return Err(Error::type_error(format!("Iterator result {} is not an object", crate::vm::display(r))));
    }
    let a_done = vm.intern("done");
    if crate::interp::to_boolean(vm.get(r, a_done)?) {
        return Ok(Value::HOLE);
    }
    let a_value = vm.a_value;
    vm.get(r, a_value)
}

/// A `for await` left early: `[iterator]`'s `return()` called, what it
/// gives (awaited after), or `undefined` without one.
fn async_iterator_close(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let it = iterator_of(a[0]);
    let a_return = vm.intern("return");
    let m = vm.get(it, a_return)?;
    if m.is_undefined() || m.is_null() {
        return Ok(Value::UNDEFINED);
    }
    call_function(vm, m, it, &[])
}

/// A direct `eval`'s scope object: `[function's scope, block-scoped count,
/// bindings' function, name, value, binding, …]`, a property for each
/// binding it sees (but an enclosing function's a `var` of the function's
/// shadows, in its scope object): where `binding` tells
/// (`(k << 4) | kind`: the bindings' function's upvalue `k`, `kind` its
/// `EVAL_CONST`… flags), an accessor of the variable itself (`value` its
/// name, a box's property's: see `codegen::boxed_lexicals`), else a data
/// property of its value (but a binding not initialized yet has none).
/// Without a prototype: the eval'd code looks names up in the function's
/// scope object after it (`%e2`, see `parse::parse_pass`). (It had the
/// function's scope object as its prototype, one of each call's: a root
/// shape and a shape for each binding per call, and their cells.)
fn eval_scope(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let proto = if crate::props::is_object(a[0]) { a[0] } else { Value::NULL };
    let o = object_value(vm.new_object(Value::NULL));
    let nlex = a[1].as_number().unwrap_or(0.0) as usize;
    let bindings = a[2];
    for (i, t) in a[3..].chunks(3).enumerate() {
        let key = to_property_key(vm, t[0])?;
        if i >= nlex && shadowed(vm, proto, key) {
            continue;
        }
        if let Some(b) = t[2].as_number()
            && crate::props::is_object(bindings)
        {
            let data = [bindings, Value::from_i32(b as i32), t[0], t[1]];
            let get = crate::promise::binding_accessor(vm, false, &data);
            let set = crate::promise::binding_accessor(vm, true, &data);
            vm.define_own_property(as_object(o), key, &Desc::accessor(get, set, true, false))?;
        } else if t[1] != Value::HOLE {
            vm.create_data_property(o, key, t[1])?;
        }
    }
    Ok(o)
}


/// A direct `eval`'s binding accessor's binding (the callee's `[function,
/// binding, name, variable]`: see `eval_scope`): its variable's upvalue,
/// its kind, its name, and (in a box) the box and its property's key.
fn binding_of(vm: &mut Vm) -> Result<(*mut crate::vm::Upvalue, i32, Value, Option<(Value, Key)>)> {
    let me = vm.callee;
    let slot = |i| crate::promise::slot(me, i);
    let (f, b, name, var) = (slot(0), slot(1), slot(2), slot(3));
    let b = b.as_number().unwrap_or(0.0) as i32;
    let u = unsafe { *Vm::upvalues(as_object(f)).add((b >> 4) as usize) };
    let boxed = if b & EVAL_BOXED as i32 != 0 { Some((unsafe { *(*u).at }, to_property_key(vm, var)?)) } else { None };
    Ok((u, b & 15, name, boxed))
}

fn uninitialized(name: Value) -> Error {
    Error::reference_error(format!("Cannot access '{}' before initialization", crate::vm::display(name)))
}

/// A direct `eval`'s binding's getter: its variable's value (a
/// ReferenceError before it is initialized).
pub(crate) fn binding_get(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let (u, _, name, boxed) = binding_of(vm)?;
    let v = match boxed {
        Some((b, key)) => vm.get_key(b, key)?,
        None => unsafe { *(*u).at },
    };
    if v == Value::HOLE {
        return Err(uninitialized(name));
    }
    Ok(v)
}

/// A direct `eval`'s binding's setter: its variable set (a TypeError for a
/// `const`'s).
pub(crate) fn binding_set(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (u, kind, name, boxed) = binding_of(vm)?;
    let old = match boxed {
        Some((b, key)) => vm.get_key(b, key)?,
        None => unsafe { *(*u).at },
    };
    if old == Value::HOLE {
        return Err(uninitialized(name));
    }
    if kind & EVAL_CONST as i32 != 0 {
        return Err(Error::type_error("Assignment to constant variable."));
    }
    let v = arg(a, 0);
    if let Some((b, key)) = boxed {
        vm.put_key(b, key, v)?;
        return Ok(Value::UNDEFINED);
    }
    unsafe {
        *(*u).at = v;
        // (A closed upvalue's variable is its own `closed`, in the heap.)
        if (*u).at == &raw mut (*u).closed {
            crate::heap::barrier(u.cast::<u8>().sub(8), (*u).at, v);
        }
    }
    Ok(Value::UNDEFINED)
}

/// The kind of `scope`'s own property `key` if it is a binding's accessor
/// (see `eval_scope`).
fn binding_kind(vm: &mut Vm, scope: Value, key: Key) -> Option<i32> {
    let d = vm.own_property(as_object(scope), key)?;
    let get = d.get.filter(|&g| crate::promise::is_binding_get(vm, g))?;
    crate::promise::slot(as_object(get), 1).as_number().map(|b| b as i32 & 15)
}

/// Whether an enclosing function's binding by `key` is shadowed by a `var`
/// of the function's an `eval` declared, in its scope object `decl`.
fn shadowed(vm: &mut Vm, decl: Value, key: Key) -> bool {
    crate::props::is_object(decl) && vm.own_property(as_object(decl), key).is_some()
}

/// After a direct `eval`: binding `[scope, name, current, decl]`'s value,
/// its scope object's own property (`current` if it has none, or for an
/// enclosing function's binding, `decl` the function's scope object, if
/// a `var` there shadows it).
fn eval_scope_get(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let key = to_property_key(vm, a[1])?;
    if shadowed(vm, a[3], key) {
        return Ok(a[2]);
    }
    Ok(match vm.own_property(as_object(a[0]), key) {
        Some(d) if d.is_data() => d.value.unwrap_or(Value::UNDEFINED),
        _ => a[2],
    })
}

/// After a direct `eval`: `[scope, function's scope, block-scoped count,
/// binding names…]`, the scope object's own properties but the bindings'
/// set on the function's scope object (the `var`s it assigned or declared
/// there, those by an enclosing function's binding's name too).
fn eval_scope_flush(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let (scope, target) = (a[0], a[1]);
    if !crate::props::is_object(target) {
        return Ok(Value::UNDEFINED);
    }
    let nlex = a[2].as_number().unwrap_or(0.0) as usize;
    let mut bindings = Vec::new();
    for (i, &n) in a[3..].iter().enumerate() {
        let key = to_property_key(vm, n)?;
        if i < nlex || !shadowed(vm, target, key) {
            bindings.push(key);
        }
    }
    for (key, _) in vm.own_keys(as_object(scope)) {
        if bindings.contains(&key) {
            continue;
        }
        if let Some(d) = vm.own_property(as_object(scope), key)
            && let Some(v) = d.value
        {
            vm.put_key(target, key, v)?;
        }
    }
    Ok(Value::UNDEFINED)
}
