//! `Proxy` (ECMA-262 §10.5, §28.2): an object whose internal methods call
//! its handler's traps, else forward to its target.
//!
//! A proxy keeps `[target, handler]` in two in-object slots beyond its
//! shape's capacity (so JS never reaches them; the collector traces them
//! by the object's own capacity), both null once revoked. One whose target
//! is callable is a native function (so calls, `typeof` and callability
//! need nothing more) whose code is [`trampoline`]'s; any other has class
//! `Proxy`. Its shape has no properties and a null prototype, and no
//! inline cache ever caches through one: once the first proxy is made
//! (`Vm::proxies_exist`), the caches' miss paths and the runtime's
//! property operations look for one on the prototype chain and, finding
//! one, take the generic paths here instead.

use crate::interp::{call_function, to_boolean};
use crate::object::{Class, Object, as_object, object_value};
use crate::props::{Desc, is_callable, is_object};
use crate::value::Value;
use crate::vm::{Error, Key, Result, Vm};

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let any = crate::jit::Ty::Any;
    let c = vm.define_native(g, "Proxy", proxy_ctor, 2, any, true);
    vm.define_native(c, "revocable", proxy_revocable, 2, any, false);
    vm.proxy_codes = [
        vm.native_function("", trampoline, 0, any, false),
        vm.native_function("", trampoline, 0, any, true),
        vm.native_function("", revoke, 0, any, false),
    ]
    .map(|f| unsafe { (*f).code });
}

/// Whether `o` is a proxy.
pub(crate) fn is_proxy(vm: &Vm, o: *mut Object) -> bool {
    unsafe {
        match (*o).class {
            Class::Proxy => true,
            Class::Native => (*o).code == vm.proxy_codes[0] || (*o).code == vm.proxy_codes[1],
            _ => false,
        }
    }
}

/// Whether `o` or an object on its prototype chain is a proxy (none
/// before the first is made).
pub(crate) fn chain_has_proxy(vm: &Vm, mut o: *mut Object) -> bool {
    if !vm.proxies_exist {
        return false;
    }
    loop {
        if is_proxy(vm, o) {
            return true;
        }
        let p = unsafe { (*(*o).shape).proto };
        if !p.is_cell() {
            return false;
        }
        o = as_object(p);
    }
}

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

/// A proxy's target and handler (a TypeError once revoked).
fn parts(o: *mut Object) -> Result<(Value, Value)> {
    let (t, h) = (slot(o, 0), slot(o, 1));
    if !t.is_cell() {
        return Err(Error::type_error("Cannot perform an operation on a revoked proxy"));
    }
    Ok((t, h))
}

/// Handler `h`'s trap `name`, if it has one.
fn trap(vm: &mut Vm, h: Value, name: &str) -> Result<Option<Value>> {
    let a = vm.intern(name);
    let m = vm.get(h, a)?;
    if m.is_undefined() || m.is_null() {
        return Ok(None);
    }
    if !is_callable(m) {
        return Err(Error::type_error(format!("The proxy's '{name}' trap is not a function")));
    }
    Ok(Some(m))
}

/// A new proxy of `target` and `handler`.
pub(crate) fn create(vm: &mut Vm, target: Value, handler: Value) -> Result<Value> {
    if !is_object(target) || !is_object(handler) {
        return Err(Error::type_error("Cannot create proxy with a non-object as target or handler"));
    }
    vm.proxies_exist = true;
    let callable = is_callable(target);
    let class = if callable { Class::Native } else { Class::Proxy };
    let full = vm.root_shape(class, Value::NULL, 2);
    let bare = vm.root_shape(class, Value::NULL, 0);
    let o = vm.heap.alloc_object(unsafe { &*full });
    unsafe {
        (*o).shape = bare;
        if callable {
            let constructs = crate::intrinsics::is_constructor(target);
            (*o).code = vm.proxy_codes[usize::from(constructs)];
        }
    }
    set_slot(o, 0, target);
    set_slot(o, 1, handler);
    Ok(object_value(o))
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// `new Proxy(target, handler)`.
fn proxy_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor Proxy requires 'new'"));
    }
    create(vm, arg(a, 0), arg(a, 1))
}

/// `Proxy.revocable(target, handler)`: `{ proxy, revoke }`.
fn proxy_revocable(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let p = create(vm, arg(a, 0), arg(a, 1))?;
    let full = vm.root_shape(Class::Native, object_value(vm.function_prototype), 1);
    let bare = vm.root_shape(Class::Native, object_value(vm.function_prototype), 0);
    let r = vm.heap.alloc_object(unsafe { &*full });
    unsafe {
        (*r).shape = bare;
        (*r).code = vm.proxy_codes[2];
    }
    set_slot(r, 0, p);
    let out = object_value(vm.new_object(object_value(vm.object_prototype)));
    let (a_proxy, a_revoke) = (vm.intern("proxy"), vm.intern("revoke"));
    vm.create_data_property(out, Key::Name(a_proxy), p)?;
    vm.create_data_property(out, Key::Name(a_revoke), object_value(r))?;
    Ok(out)
}

/// A revocable proxy's `revoke()`: its proxy's target and handler gone.
fn revoke(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let r = vm.callee;
    let p = slot(r, 0);
    if p.is_cell() {
        set_slot(as_object(p), 0, Value::NULL);
        set_slot(as_object(p), 1, Value::NULL);
        set_slot(r, 0, Value::NULL);
    }
    Ok(Value::UNDEFINED)
}

/// A callable proxy called (`apply`) or constructed (`construct`).
fn trampoline(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    let constructing = vm.is_construct();
    let p = vm.callee;
    let nt = std::mem::replace(&mut vm.new_target, Value::EMPTY);
    let (t, h) = parts(p)?;
    let list = crate::array::new_array_of(vm, args)?;
    if constructing {
        let nt = if nt == Value::EMPTY { object_value(p) } else { nt };
        let Some(f) = trap(vm, h, "construct")? else {
            return crate::intrinsics::construct_with(vm, t, args, nt);
        };
        let r = call_function(vm, f, h, &[t, list, nt])?;
        if !is_object(r) {
            return Err(Error::type_error("The proxy's 'construct' trap returned a non-object"));
        }
        return Ok(r);
    }
    match trap(vm, h, "apply")? {
        Some(f) => call_function(vm, f, h, &[t, this, list]),
        None => call_function(vm, t, this, args),
    }
}

// ---- The internal methods of proxies ----

fn get(vm: &mut Vm, p: *mut Object, key: Key, receiver: Value) -> Result<Value> {
    let (t, h) = parts(p)?;
    match trap(vm, h, "get")? {
        Some(f) => {
            let k = vm.key_value(key);
            call_function(vm, f, h, &[t, k, receiver])
        }
        None => generic_get(vm, t, key, receiver),
    }
}

fn set(vm: &mut Vm, p: *mut Object, key: Key, v: Value, receiver: Value) -> Result<bool> {
    let (t, h) = parts(p)?;
    match trap(vm, h, "set")? {
        Some(f) => {
            let k = vm.key_value(key);
            Ok(to_boolean(call_function(vm, f, h, &[t, k, v, receiver])?))
        }
        None => generic_set(vm, t, key, v, receiver),
    }
}

fn has(vm: &mut Vm, p: *mut Object, key: Key) -> Result<bool> {
    let (t, h) = parts(p)?;
    match trap(vm, h, "has")? {
        Some(f) => {
            let k = vm.key_value(key);
            Ok(to_boolean(call_function(vm, f, h, &[t, k])?))
        }
        None => generic_has(vm, t, key),
    }
}

// ---- Proxy-aware internal methods of any object ----

/// `o.[[Get]](key, receiver)`.
pub(crate) fn generic_get(vm: &mut Vm, o: Value, key: Key, receiver: Value) -> Result<Value> {
    let mut cur = as_object(o);
    loop {
        if is_proxy(vm, cur) {
            return get(vm, cur, key, receiver);
        }
        if let Some(d) = vm.own_property(cur, key) {
            if d.is_accessor() {
                return match d.get.filter(|g| !g.is_undefined()) {
                    Some(g) => call_function(vm, g, receiver, &[]),
                    None => Ok(Value::UNDEFINED),
                };
            }
            return Ok(d.value.unwrap_or(Value::UNDEFINED));
        }
        let p = unsafe { (*(*cur).shape).proto };
        if !p.is_cell() {
            return Ok(Value::UNDEFINED);
        }
        cur = as_object(p);
    }
}

/// `o.[[Set]](key, v, receiver)`: whether it was set.
pub(crate) fn generic_set(vm: &mut Vm, o: Value, key: Key, v: Value, receiver: Value) -> Result<bool> {
    let mut cur = as_object(o);
    let found = loop {
        if is_proxy(vm, cur) {
            return set(vm, cur, key, v, receiver);
        }
        if let Some(d) = vm.own_property(cur, key) {
            break Some(d);
        }
        let p = unsafe { (*(*cur).shape).proto };
        if !p.is_cell() {
            break None;
        }
        cur = as_object(p);
    };
    match found {
        Some(d) if d.is_accessor() => match d.set.filter(|s| !s.is_undefined()) {
            Some(s) => {
                call_function(vm, s, receiver, &[v])?;
                Ok(true)
            }
            None => Ok(false),
        },
        Some(d) if d.writable == Some(false) => Ok(false),
        _ => {
            if !is_object(receiver) {
                return Ok(false);
            }
            let r = as_object(receiver);
            match get_own_property(vm, r, key)? {
                Some(e) if e.is_accessor() || e.writable == Some(false) => Ok(false),
                Some(_) => define_property(vm, r, key, &Desc { value: Some(v), ..Desc::default() }),
                None => define_property(vm, r, key, &Desc::data(v, true, true, true)),
            }
        }
    }
}

/// `o.[[HasProperty]](key)`.
pub(crate) fn generic_has(vm: &mut Vm, o: Value, key: Key) -> Result<bool> {
    let mut cur = as_object(o);
    loop {
        if is_proxy(vm, cur) {
            return has(vm, cur, key);
        }
        if vm.own_property(cur, key).is_some() {
            return Ok(true);
        }
        let p = unsafe { (*(*cur).shape).proto };
        if !p.is_cell() {
            return Ok(false);
        }
        cur = as_object(p);
    }
}

/// `o.[[Delete]](key)`: whether it's gone.
pub(crate) fn delete(vm: &mut Vm, o: *mut Object, key: Key) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(vm.delete_own(o, key));
    }
    let (t, h) = parts(o)?;
    match trap(vm, h, "deleteProperty")? {
        Some(f) => {
            let k = vm.key_value(key);
            Ok(to_boolean(call_function(vm, f, h, &[t, k])?))
        }
        None => delete(vm, as_object(t), key),
    }
}

/// `o.[[GetOwnProperty]](key)`.
pub(crate) fn get_own_property(vm: &mut Vm, o: *mut Object, key: Key) -> Result<Option<Desc>> {
    if !is_proxy(vm, o) {
        return Ok(vm.own_property(o, key));
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "getOwnPropertyDescriptor")? else {
        return get_own_property(vm, as_object(t), key);
    };
    let k = vm.key_value(key);
    let r = call_function(vm, f, h, &[t, k])?;
    if r.is_undefined() {
        return Ok(None);
    }
    if !is_object(r) {
        return Err(Error::type_error("The proxy's 'getOwnPropertyDescriptor' trap returned neither an object nor undefined"));
    }
    let mut d = vm.to_descriptor(r)?;
    // (Completed: absent fields take their defaults.)
    if d.is_accessor() {
        d.get.get_or_insert(Value::UNDEFINED);
        d.set.get_or_insert(Value::UNDEFINED);
    } else {
        d.value.get_or_insert(Value::UNDEFINED);
        d.writable.get_or_insert(false);
    }
    d.enumerable.get_or_insert(false);
    d.configurable.get_or_insert(false);
    Ok(Some(d))
}

/// `o.[[DefineOwnProperty]](key, d)`: whether it was defined.
pub(crate) fn define_property(vm: &mut Vm, o: *mut Object, key: Key, d: &Desc) -> Result<bool> {
    if !is_proxy(vm, o) {
        return vm.define_own_property(o, key, d);
    }
    proxy_define(vm, o, key, d)
}

/// A proxy's `[[DefineOwnProperty]]`.
pub(crate) fn proxy_define(vm: &mut Vm, o: *mut Object, key: Key, d: &Desc) -> Result<bool> {
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "defineProperty")? else {
        return define_property(vm, as_object(t), key, d);
    };
    let k = vm.key_value(key);
    let dv = from_partial_descriptor(vm, d);
    Ok(to_boolean(call_function(vm, f, h, &[t, k, dv])?))
}

/// A descriptor object of just the fields `d` has.
fn from_partial_descriptor(vm: &mut Vm, d: &Desc) -> Value {
    let o = object_value(vm.new_object(object_value(vm.object_prototype)));
    let fields = [
        ("value", d.value),
        ("writable", d.writable.map(Value::from_bool)),
        ("get", d.get),
        ("set", d.set),
        ("enumerable", d.enumerable.map(Value::from_bool)),
        ("configurable", d.configurable.map(Value::from_bool)),
    ];
    for (name, v) in fields {
        if let Some(v) = v {
            let a = vm.intern(name);
            let _ = vm.create_data_property(o, Key::Name(a), v);
        }
    }
    o
}

/// `o.[[OwnPropertyKeys]]()`: indices, names, then symbols.
pub(crate) fn own_keys(vm: &mut Vm, o: *mut Object) -> Result<Vec<Key>> {
    if !is_proxy(vm, o) {
        let mut keys: Vec<Key> = vm.own_keys(o).into_iter().map(|(k, _)| k).collect();
        for s in crate::symbol::own_symbol_keys(vm, o) {
            keys.push(Key::Name(crate::symbol::symbol_atom(s)));
        }
        return Ok(keys);
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "ownKeys")? else {
        return own_keys(vm, as_object(t));
    };
    let r = call_function(vm, f, h, &[t])?;
    if !is_object(r) {
        return Err(Error::type_error("The proxy's 'ownKeys' trap returned a non-object"));
    }
    let n = vm.length_of(r)?;
    let mut keys = Vec::new();
    let mut i = 0.0;
    while i < n {
        let k = vm.get_keyed(r, Value::number(i))?;
        if crate::vm::string_units(k).is_none() && !crate::symbol::is_symbol(k) {
            return Err(Error::type_error("The proxy's 'ownKeys' trap gave a key neither a string nor a symbol"));
        }
        let key = crate::intrinsics::to_property_key(vm, k)?;
        if keys.contains(&key) {
            return Err(Error::type_error("The proxy's 'ownKeys' trap gave a key twice"));
        }
        keys.push(key);
        i += 1.0;
    }
    Ok(keys)
}

/// `o`'s own enumerable string keys (not symbols), in order: what
/// `Object.keys`, `for … in` and `JSON.stringify` list.
pub(crate) fn enumerable_own_keys(vm: &mut Vm, o: *mut Object) -> Result<Vec<Key>> {
    let mut out = Vec::new();
    for key in own_keys(vm, o)? {
        if let Key::Name(a) = key
            && crate::symbol::is_symbol_atom(vm, a)
        {
            continue;
        }
        if get_own_property(vm, o, key)?.is_some_and(|d| d.enumerable == Some(true)) {
            out.push(key);
        }
    }
    Ok(out)
}

/// `o.[[GetPrototypeOf]]()`.
pub(crate) fn get_prototype_of(vm: &mut Vm, o: *mut Object) -> Result<Value> {
    if !is_proxy(vm, o) {
        return Ok(unsafe { (*(*o).shape).proto });
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "getPrototypeOf")? else {
        return get_prototype_of(vm, as_object(t));
    };
    let r = call_function(vm, f, h, &[t])?;
    if !is_object(r) && !r.is_null() {
        return Err(Error::type_error("The proxy's 'getPrototypeOf' trap returned neither an object nor null"));
    }
    Ok(r)
}

/// `o.[[SetPrototypeOf]](proto)`: whether it was set.
pub(crate) fn set_prototype_of(vm: &mut Vm, o: *mut Object, proto: Value) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(vm.set_prototype_of(o, proto));
    }
    let (t, h) = parts(o)?;
    match trap(vm, h, "setPrototypeOf")? {
        Some(f) => Ok(to_boolean(call_function(vm, f, h, &[t, proto])?)),
        None => set_prototype_of(vm, as_object(t), proto),
    }
}

/// `o.[[IsExtensible]]()`.
pub(crate) fn is_extensible(vm: &mut Vm, o: *mut Object) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(Vm::is_extensible(o));
    }
    let (t, h) = parts(o)?;
    match trap(vm, h, "isExtensible")? {
        Some(f) => Ok(to_boolean(call_function(vm, f, h, &[t])?)),
        None => is_extensible(vm, as_object(t)),
    }
}

/// `o.[[PreventExtensions]]()`: whether it now isn't extensible.
pub(crate) fn prevent_extensions(vm: &mut Vm, o: *mut Object) -> Result<bool> {
    if !is_proxy(vm, o) {
        vm.prevent_extensions(o);
        return Ok(true);
    }
    let (t, h) = parts(o)?;
    match trap(vm, h, "preventExtensions")? {
        Some(f) => Ok(to_boolean(call_function(vm, f, h, &[t])?)),
        None => prevent_extensions(vm, as_object(t)),
    }
}

/// The target a proxy (or proxy of a proxy …) stands for at the end, for
/// `Array.isArray` (a TypeError once revoked).
pub(crate) fn final_target(vm: &Vm, mut o: *mut Object) -> Result<*mut Object> {
    while is_proxy(vm, o) {
        o = as_object(parts(o)?.0);
    }
    Ok(o)
}

/// `for … in`'s keys of `o`: its own enumerable string keys, then its
/// prototypes' not seen before, through proxies' traps.
pub(crate) fn for_in_names(vm: &mut Vm, o: *mut Object) -> Result<Vec<Vec<u16>>> {
    let mut seen: Vec<Key> = Vec::new();
    let mut out = Vec::new();
    let mut cur = o;
    loop {
        for key in own_keys(vm, cur)? {
            if matches!(key, Key::Name(a) if crate::symbol::is_symbol_atom(vm, a)) || seen.contains(&key) {
                continue;
            }
            seen.push(key);
            if get_own_property(vm, cur, key)?.is_some_and(|d| d.enumerable == Some(true)) {
                let s = match key {
                    Key::Index(i) => i.to_string(),
                    Key::Name(a) => vm.atom_name(a).to_string(),
                };
                out.push(s.encode_utf16().collect());
            }
        }
        let p = get_prototype_of(vm, cur)?;
        if !p.is_cell() {
            return Ok(out);
        }
        cur = as_object(p);
    }
}

/// IsArray(`v`): an array, or a proxy for one (a TypeError once revoked).
pub(crate) fn is_array(vm: &Vm, v: Value) -> Result<bool> {
    if !is_object(v) {
        return Ok(false);
    }
    let o = final_target(vm, as_object(v))?;
    Ok(unsafe { (*o).class } == Class::Array)
}
