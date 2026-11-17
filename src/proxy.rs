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
//!
//! A trap's result is checked against its target (the invariants of
//! §10.5.*: a non-configurable property can't be reported otherwise than
//! the target has it, nor a non-extensible target otherwise than it is).

use crate::interp::{call_function, to_boolean};
use crate::jit::HashSet;
use crate::object::{Class, Obj, Object, object_value};
use crate::props::{Desc, is_callable, is_object, same_value};
use crate::raw::{Live, ObjectPtr};
use crate::value::Value;
use crate::vm::{Error, Key, Result, Vm};

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global_obj();
    let any = crate::jit::Ty::Any;
    let c = vm.define_native(g, "Proxy", proxy_ctor, 2, any, true);
    vm.define_native(c, "revocable", proxy_revocable, 2, any, false);
    vm.proxy_codes = [
        vm.native_function("", trampoline, 0, any, false),
        vm.native_function("", trampoline, 0, any, true),
        vm.native_function("", revoke, 0, any, false),
    ]
    .map(|f| f.code());
    // (A proxy's call runs in its caller's realm: see `realm`.)
    for c in vm.proxy_codes {
        unsafe { (*c.cast_mut()).realm = crate::realm::NONE };
    }
}

/// Whether `o` is a proxy.
pub(crate) fn is_proxy(vm: &Vm, o: Obj) -> bool {
    match o.class() {
        Class::Proxy => true,
        Class::Native => o.code() == vm.proxy_codes[0] || o.code() == vm.proxy_codes[1],
        _ => false,
    }
}

/// Whether `o` or an object on its prototype chain is a proxy (none
/// before the first is made).
pub(crate) fn chain_has_proxy(vm: &Vm, mut o: Obj) -> bool {
    if !vm.proxies_exist {
        return false;
    }
    loop {
        if is_proxy(vm, o) {
            return true;
        }
        let p = o.shape().live().proto;
        if !p.is_cell() {
            return false;
        }
        o = Obj::of(p);
    }
}

fn slot(o: Obj, i: usize) -> Value {
    unsafe { *Object::inline(o.ptr(), i) }
}

fn set_slot(o: Obj, i: usize, v: Value) {
    unsafe {
        let at = Object::inline(o.ptr(), i);
        crate::heap::store(o.ptr().cast(), at, v);
    }
}

/// A proxy's target and handler (a TypeError once revoked).
fn parts(o: Obj) -> Result<(Value, Value)> {
    let (t, h) = (slot(o, 0), slot(o, 1));
    if !t.is_cell() {
        return Err(Error::type_error("Cannot perform an operation on a revoked proxy"));
    }
    Ok((t, h))
}

/// A key as a trap gets it: a property key, a String or a Symbol (an index
/// too, as its string: not the number `key_value` makes it for access).
fn trap_key(vm: &mut Vm, key: Key) -> Value {
    match key {
        Key::Index(_) => vm.key_string(key),
        Key::Name(_) => vm.key_value(key),
    }
}

/// Handler `h`'s trap `name`, if it has one.
/// Every internal method of a proxy looks its trap up first: a chain of
/// proxies forwarding to their targets recurses through here, near the
/// thread's stack's end a RangeError. (Not `Vm::native_limit`, the run's
/// budget for compiled calls: a module namespace, a proxy, is read deep
/// in a deep graph's evaluation.)
fn trap(vm: &mut Vm, h: Value, name: &str) -> Result<Option<Value>> {
    if crate::stack::low() {
        return Err(Error::range_error("Maximum call stack size exceeded"));
    }
    let a = vm.intern_name(name);
    let m = vm.get(h, a)?;
    if m.is_undefined() || m.is_null() {
        return Ok(None);
    }
    if !is_callable(m) {
        return Err(Error::type_error(format!("The proxy's '{name}' trap is not a function")));
    }
    Ok(Some(m))
}

/// A trap's result that breaks an invariant: a TypeError.
fn broken(name: &str, what: &str) -> Error {
    Error::type_error(format!("The proxy's '{name}' trap {what}"))
}

/// IsCompatiblePropertyDescriptor: whether defining `d` on an object
/// whose property is `cur` (none: `None`), extensible if `extensible`,
/// would be allowed.
fn compatible(extensible: bool, d: &Desc, cur: Option<&Desc>) -> bool {
    let Some(cur) = cur else { return extensible };
    if cur.configurable != Some(false) {
        return true;
    }
    if d.configurable == Some(true) || d.enumerable.is_some() && d.enumerable != cur.enumerable {
        return false;
    }
    if (d.is_accessor() || d.is_data()) && d.is_accessor() != cur.is_accessor() {
        return false;
    }
    let same = |a: Option<Value>, b: Option<Value>| a.is_none_or(|a| same_value(a, b.unwrap_or(Value::UNDEFINED)));
    if cur.is_accessor() {
        return same(d.get, cur.get) && same(d.set, cur.set);
    }
    cur.writable != Some(false) || d.writable != Some(true) && same(d.value, cur.value)
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
    let o = unsafe { Obj::from_ptr(vm.heap.alloc_object(full.live())) };
    unsafe {
        crate::object::set_shape(o.ptr(), bare);
        if callable {
            let constructs = crate::intrinsics::is_constructor(target);
            (*o.ptr()).code = vm.proxy_codes[usize::from(constructs)];
        }
    }
    set_slot(o, 0, target);
    set_slot(o, 1, handler);
    Ok(object_value(o.ptr()))
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
    let r = vm.heap.alloc_object(full.live());
    unsafe {
        crate::object::set_shape(r, bare);
        (*r).code = vm.proxy_codes[2];
    }
    set_slot(unsafe { Obj::from_ptr(r) }, 0, p);
    let out = object_value((vm.new_plain(object_value(vm.object_prototype))).ptr());
    let (a_proxy, a_revoke) = (vm.intern_name("proxy"), vm.intern_name("revoke"));
    vm.create_data_property(out, Key::Name(a_proxy), p)?;
    vm.create_data_property(out, Key::Name(a_revoke), object_value(r))?;
    Ok(out)
}

/// A revocable proxy's `revoke()`: its proxy's target and handler gone.
fn revoke(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let r = vm.callee_obj();
    let p = slot(r, 0);
    if p.is_cell() {
        set_slot(Obj::of(p), 0, Value::NULL);
        set_slot(Obj::of(p), 1, Value::NULL);
        set_slot(r, 0, Value::NULL);
    }
    Ok(Value::UNDEFINED)
}

/// A callable proxy called (`apply`) or constructed (`construct`).
fn trampoline(vm: &mut Vm, this: Value, args: &[Value]) -> Result<Value> {
    let constructing = vm.is_construct();
    let p = vm.callee_obj();
    let nt = std::mem::replace(&mut vm.new_target, Value::EMPTY);
    let (t, h) = parts(p)?;
    let list = crate::array::new_array_of(vm, args)?;
    if constructing {
        let nt = if nt == Value::EMPTY { object_value(p.ptr()) } else { nt };
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

/// A proxy's `[[Get]]` by its handler `h`'s `get` trap `f`, its target
/// `t` (one without the trap forwards in `generic_get`'s loop).
fn get(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key, receiver: Value) -> Result<Value> {
    let k = trap_key(vm, key);
    let v = call_function(vm, f, h, &[t, k, receiver])?;
    if let Some(d) = get_own_property(vm, Obj::of(t), key)?
        && d.configurable == Some(false)
    {
        if d.is_data() && d.writable == Some(false) && !same_value(v, d.value.unwrap_or(Value::UNDEFINED)) {
            return Err(broken("get", "reported a non-writable, non-configurable property's value otherwise"));
        }
        if d.is_accessor() && d.get.is_none_or(|g| g.is_undefined()) && !v.is_undefined() {
            return Err(broken("get", "reported a value for a non-configurable property without a getter"));
        }
    }
    Ok(v)
}

/// A proxy's `[[Set]]` by its `set` trap `f` (see [`get`]).
fn set(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key, v: Value, receiver: Value) -> Result<bool> {
    let k = trap_key(vm, key);
    if !to_boolean(call_function(vm, f, h, &[t, k, v, receiver])?) {
        return Ok(false);
    }
    if let Some(d) = get_own_property(vm, Obj::of(t), key)?
        && d.configurable == Some(false)
    {
        if d.is_data() && d.writable == Some(false) && !same_value(v, d.value.unwrap_or(Value::UNDEFINED)) {
            return Err(broken("set", "set a non-writable, non-configurable property otherwise"));
        }
        if d.is_accessor() && d.set.is_none_or(|s| s.is_undefined()) {
            return Err(broken("set", "set a non-configurable property without a setter"));
        }
    }
    Ok(true)
}

/// A proxy's `[[HasProperty]]` by its `has` trap `f` (see [`get`]).
fn has(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key) -> Result<bool> {
    let k = trap_key(vm, key);
    if to_boolean(call_function(vm, f, h, &[t, k])?) {
        return Ok(true);
    }
    if let Some(d) = get_own_property(vm, Obj::of(t), key)? {
        if d.configurable == Some(false) {
            return Err(broken("has", "hid a non-configurable property"));
        }
        if !is_extensible(vm, Obj::of(t))? {
            return Err(broken("has", "hid a property of a non-extensible target"));
        }
    }
    Ok(false)
}

// ---- Proxy-aware internal methods of any object ----

/// How many proxies without the trap a property lookup goes through
/// before it gives up with a RangeError. A proxy without the trap
/// forwards to its target in the lookup's loop, not by a call, so a
/// cycle through one (OrdinarySetPrototypeOf stops its check at a proxy,
/// 10.1.2.1 step 7.b.i) would loop forever, the stack never growing to
/// its limit (issue #336). Other engines recurse and run out of stack:
/// JavaScriptCore past 10^4 nested proxies, SpiderMonkey and QuickJS
/// before.
const MAX_FORWARDS: u32 = 100_000;

/// A proxy `p`'s target and handler, and its trap `name` if it has one,
/// a forward counted in `hops`.
fn forward(vm: &mut Vm, p: Obj, name: &str, hops: &mut u32) -> Result<(Value, Value, Option<Value>)> {
    *hops += 1;
    if *hops > MAX_FORWARDS {
        return Err(Error::range_error("Maximum call stack size exceeded"));
    }
    let (t, h) = parts(p)?;
    Ok((t, h, trap(vm, h, name)?))
}

/// `o.[[Get]](key, receiver)`.
pub(crate) fn generic_get(vm: &mut Vm, o: Value, key: Key, receiver: Value) -> Result<Value> {
    let mut cur = Obj::of(o);
    let mut hops = 0;
    loop {
        if is_proxy(vm, cur) {
            match forward(vm, cur, "get", &mut hops)? {
                (t, h, Some(f)) => return get(vm, t, h, f, key, receiver),
                (t, _, None) => {
                    cur = Obj::of(t);
                    continue;
                }
            }
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
        let p = unsafe { Obj::from_ptr(cur.ptr()) }.shape().live().proto;
        if !p.is_cell() {
            return Ok(Value::UNDEFINED);
        }
        cur = Obj::of(p);
    }
}

/// `o.[[Set]](key, v, receiver)`: whether it was set.
pub(crate) fn generic_set(vm: &mut Vm, o: Value, key: Key, v: Value, receiver: Value) -> Result<bool> {
    let mut cur = Obj::of(o);
    let mut hops = 0;
    let found = loop {
        if is_proxy(vm, cur) {
            match forward(vm, cur, "set", &mut hops)? {
                (t, h, Some(f)) => return set(vm, t, h, f, key, v, receiver),
                (t, _, None) => {
                    cur = Obj::of(t);
                    continue;
                }
            }
        }
        if let Some(d) = vm.own_property(cur, key) {
            break Some(d);
        }
        let p = unsafe { Obj::from_ptr(cur.ptr()) }.shape().live().proto;
        if !p.is_cell() {
            break None;
        }
        cur = Obj::of(p);
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
            let r = Obj::of(receiver);
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
    let mut cur = Obj::of(o);
    let mut hops = 0;
    loop {
        if is_proxy(vm, cur) {
            match forward(vm, cur, "has", &mut hops)? {
                (t, h, Some(f)) => return has(vm, t, h, f, key),
                (t, _, None) => {
                    cur = Obj::of(t);
                    continue;
                }
            }
        }
        if vm.own_property(cur, key).is_some() {
            return Ok(true);
        }
        let p = unsafe { Obj::from_ptr(cur.ptr()) }.shape().live().proto;
        if !p.is_cell() {
            return Ok(false);
        }
        cur = Obj::of(p);
    }
}

/// `o.[[Delete]](key)`: whether it's gone.
pub(crate) fn delete(vm: &mut Vm, o: Obj, key: Key) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(vm.delete_own(o, key));
    }
    let (t, h) = parts(o)?;
    match trap(vm, h, "deleteProperty")? {
        Some(f) => {
            let k = trap_key(vm, key);
            if !to_boolean(call_function(vm, f, h, &[t, k])?) {
                return Ok(false);
            }
            if let Some(d) = get_own_property(vm, Obj::of(t), key)? {
                if d.configurable == Some(false) {
                    return Err(broken("deleteProperty", "deleted a non-configurable property"));
                }
                if !is_extensible(vm, Obj::of(t))? {
                    return Err(broken("deleteProperty", "deleted a property of a non-extensible target"));
                }
            }
            Ok(true)
        }
        None => delete(vm, Obj::of(t), key),
    }
}

/// `o.[[GetOwnProperty]](key)`.
pub(crate) fn get_own_property(vm: &mut Vm, o: Obj, key: Key) -> Result<Option<Desc>> {
    if !is_proxy(vm, o) {
        return Ok(vm.own_property(o, key));
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "getOwnPropertyDescriptor")? else {
        return get_own_property(vm, Obj::of(t), key);
    };
    let k = trap_key(vm, key);
    let r = call_function(vm, f, h, &[t, k])?;
    if !r.is_undefined() && !is_object(r) {
        return Err(Error::type_error(
            "The proxy's 'getOwnPropertyDescriptor' trap returned neither an object nor undefined",
        ));
    }
    let target = get_own_property(vm, Obj::of(t), key)?;
    if r.is_undefined() {
        if let Some(td) = target {
            if td.configurable == Some(false) {
                return Err(broken("getOwnPropertyDescriptor", "hid a non-configurable property"));
            }
            if !is_extensible(vm, Obj::of(t))? {
                return Err(broken("getOwnPropertyDescriptor", "hid a property of a non-extensible target"));
            }
        }
        return Ok(None);
    }
    let extensible = is_extensible(vm, Obj::of(t))?;
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
    if !compatible(extensible, &d, target.as_ref()) {
        return Err(broken("getOwnPropertyDescriptor", "reported a property incompatible with the target's"));
    }
    if d.configurable == Some(false) {
        match target {
            Some(td) if td.configurable == Some(false) => {
                if d.writable == Some(false) && td.writable == Some(true) {
                    return Err(broken("getOwnPropertyDescriptor", "reported a writable property as non-writable"));
                }
            }
            _ => {
                return Err(broken(
                    "getOwnPropertyDescriptor",
                    "reported a configurable or missing property as non-configurable",
                ));
            }
        }
    }
    Ok(Some(d))
}

/// `o.[[DefineOwnProperty]](key, d)`: whether it was defined.
pub(crate) fn define_property(vm: &mut Vm, o: Obj, key: Key, d: &Desc) -> Result<bool> {
    if !is_proxy(vm, o) {
        return vm.define_own_property(o, key, d);
    }
    proxy_define(vm, o, key, d)
}

/// A proxy's `[[DefineOwnProperty]]`.
pub(crate) fn proxy_define(vm: &mut Vm, o: Obj, key: Key, d: &Desc) -> Result<bool> {
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "defineProperty")? else {
        return define_property(vm, Obj::of(t), key, d);
    };
    let k = trap_key(vm, key);
    let dv = from_partial_descriptor(vm, d);
    if !to_boolean(call_function(vm, f, h, &[t, k, dv])?) {
        return Ok(false);
    }
    let target = get_own_property(vm, Obj::of(t), key)?;
    let extensible = is_extensible(vm, Obj::of(t))?;
    let config_false = d.configurable == Some(false);
    match target {
        None if !extensible => Err(broken("defineProperty", "added a property to a non-extensible target")),
        None if config_false => Err(broken("defineProperty", "defined a non-configurable property the target lacks")),
        None => Ok(true),
        Some(td) => {
            if !compatible(extensible, d, Some(&td)) {
                return Err(broken("defineProperty", "defined a property incompatible with the target's"));
            }
            if config_false && td.configurable == Some(true) {
                return Err(broken(
                    "defineProperty",
                    "defined a property configurable on the target as non-configurable",
                ));
            }
            if td.is_data() && td.configurable == Some(false) && td.writable == Some(true) && d.writable == Some(false)
            {
                return Err(broken("defineProperty", "made a writable, non-configurable property non-writable"));
            }
            Ok(true)
        }
    }
}

/// A descriptor object of just the fields `d` has.
fn from_partial_descriptor(vm: &mut Vm, d: &Desc) -> Value {
    let o = object_value((vm.new_plain(object_value(vm.object_prototype))).ptr());
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
            let a = vm.intern_name(name);
            let _ = vm.create_data_property(o, Key::Name(a), v);
        }
    }
    o
}

/// `o.[[OwnPropertyKeys]]()`: indices, names, then symbols.
pub(crate) fn own_keys(vm: &mut Vm, o: Obj) -> Result<Vec<Key>> {
    if !is_proxy(vm, o) {
        let mut keys: Vec<Key> = vm.own_keys(o).into_iter().map(|(k, _)| k).collect();
        for s in crate::symbol::own_symbol_keys(vm, o) {
            keys.push(Key::Name(crate::symbol::symbol_atom(vm, s)));
        }
        return Ok(keys);
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "ownKeys")? else {
        return own_keys(vm, Obj::of(t));
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
        if !crate::vm::is_string(k) && !crate::symbol::is_symbol(k) {
            return Err(Error::type_error("The proxy's 'ownKeys' trap gave a key neither a string nor a symbol"));
        }
        keys.push(crate::intrinsics::to_property_key(vm, k)?);
        i += 1.0;
    }
    // (Once every element is read: CreateListFromArrayLike, then 10.5.11
    // step 9. Hashed, as is what the target's keys are checked against
    // below: searching the list made a proxy of n keys O(n²).)
    let mut unchecked: HashSet<Key> = HashSet::default();
    if keys.iter().any(|&k| !unchecked.insert(k)) {
        return Err(Error::type_error("The proxy's 'ownKeys' trap gave a key twice"));
    }
    // Every non-configurable key of the target's (and of a non-extensible
    // one's, every key and no other).
    let target = Obj::of(t);
    let extensible = is_extensible(vm, target)?;
    let (mut fixed, mut other) = (Vec::new(), Vec::new());
    for k in own_keys(vm, target)? {
        match get_own_property(vm, target, k)? {
            Some(d) if d.configurable == Some(false) => fixed.push(k),
            _ => other.push(k),
        }
    }
    if extensible && fixed.is_empty() {
        return Ok(keys);
    }
    for k in fixed {
        if !unchecked.remove(&k) {
            return Err(broken("ownKeys", "omitted a non-configurable key"));
        }
    }
    if extensible {
        return Ok(keys);
    }
    for k in other {
        if !unchecked.remove(&k) {
            return Err(broken("ownKeys", "omitted a key of a non-extensible target"));
        }
    }
    if !unchecked.is_empty() {
        return Err(broken("ownKeys", "added a key to a non-extensible target"));
    }
    Ok(keys)
}

/// `o`'s own enumerable string keys (not symbols), in order: what
/// `Object.keys`, `for … in` and `JSON.stringify` list.
pub(crate) fn enumerable_own_keys(vm: &mut Vm, o: Obj) -> Result<Vec<Key>> {
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
pub(crate) fn get_prototype_of(vm: &mut Vm, o: Obj) -> Result<Value> {
    if !is_proxy(vm, o) {
        return Ok(o.shape().live().proto);
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "getPrototypeOf")? else {
        return get_prototype_of(vm, Obj::of(t));
    };
    let r = call_function(vm, f, h, &[t])?;
    if !is_object(r) && !r.is_null() {
        return Err(Error::type_error("The proxy's 'getPrototypeOf' trap returned neither an object nor null"));
    }
    if !is_extensible(vm, Obj::of(t))? && !same_value(r, get_prototype_of(vm, Obj::of(t))?) {
        return Err(broken("getPrototypeOf", "reported a non-extensible target's prototype otherwise"));
    }
    Ok(r)
}

/// `o.[[SetPrototypeOf]](proto)`: whether it was set.
pub(crate) fn set_prototype_of(vm: &mut Vm, o: Obj, proto: Value) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(vm.set_prototype_of(o, proto));
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "setPrototypeOf")? else {
        return set_prototype_of(vm, Obj::of(t), proto);
    };
    if !to_boolean(call_function(vm, f, h, &[t, proto])?) {
        return Ok(false);
    }
    if !is_extensible(vm, Obj::of(t))? && !same_value(proto, get_prototype_of(vm, Obj::of(t))?) {
        return Err(broken("setPrototypeOf", "changed a non-extensible target's prototype"));
    }
    Ok(true)
}

/// `o.[[IsExtensible]]()`.
pub(crate) fn is_extensible(vm: &mut Vm, o: Obj) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(Vm::is_extensible(o));
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "isExtensible")? else {
        return is_extensible(vm, Obj::of(t));
    };
    let r = to_boolean(call_function(vm, f, h, &[t])?);
    if r != is_extensible(vm, Obj::of(t))? {
        return Err(broken("isExtensible", "reported the target's extensibility otherwise"));
    }
    Ok(r)
}

/// `o.[[PreventExtensions]]()`: whether it now isn't extensible.
pub(crate) fn prevent_extensions(vm: &mut Vm, o: Obj) -> Result<bool> {
    if !is_proxy(vm, o) {
        vm.prevent_extensions(o);
        return Ok(true);
    }
    let (t, h) = parts(o)?;
    let Some(f) = trap(vm, h, "preventExtensions")? else {
        return prevent_extensions(vm, Obj::of(t));
    };
    let r = to_boolean(call_function(vm, f, h, &[t])?);
    if r && is_extensible(vm, Obj::of(t))? {
        return Err(broken("preventExtensions", "reported an extensible target as not"));
    }
    Ok(r)
}

/// The target a proxy (or proxy of a proxy …) stands for at the end, for
/// `Array.isArray` (a TypeError once revoked).
pub(crate) fn final_target(vm: &Vm, mut o: Obj) -> Result<Obj> {
    while is_proxy(vm, o) {
        o = Obj::of(parts(o)?.0);
    }
    Ok(o)
}

/// `for … in`'s keys of `o`: its own enumerable string keys, then its
/// prototypes' not seen before, through proxies' traps.
pub(crate) fn for_in_names(vm: &mut Vm, o: Obj) -> Result<Vec<Vec<u16>>> {
    // This follows the same shadowing rule as the ordinary path in
    // `Vm::for_in_key_array`; proxy traps must not turn it quadratic.
    let mut seen: HashSet<Key> = HashSet::default();
    let mut out = Vec::new();
    let mut cur = o;
    loop {
        for key in own_keys(vm, cur)? {
            if matches!(key, Key::Name(a) if crate::symbol::is_symbol_atom(vm, a)) || seen.contains(&key) {
                continue;
            }
            // (A key a trap lists without a property hides nothing: only
            // one with a descriptor joins VisitedKeys, 14.7.5.10.2.1.)
            let Some(d) = get_own_property(vm, cur, key)? else {
                continue;
            };
            seen.insert(key);
            if d.enumerable == Some(true) {
                let mut u = Vec::new();
                match key {
                    Key::Index(i) => u.extend(i.to_string().encode_utf16()),
                    Key::Name(a) => crate::js::lex::string_units(vm.atom_name(a), &mut u),
                }
                out.push(u);
            }
        }
        let p = get_prototype_of(vm, cur)?;
        if !p.is_cell() {
            return Ok(out);
        }
        cur = Obj::of(p);
    }
}

/// IsArray(`v`): an array, or a proxy for one (a TypeError once revoked).
pub(crate) fn is_array(vm: &Vm, v: Value) -> Result<bool> {
    if !is_object(v) {
        return Ok(false);
    }
    let o = final_target(vm, Obj::of(v))?;
    Ok(o.class() == Class::Array)
}

/// Proxy `o`'s target (a TypeError once revoked).
pub(crate) fn target(o: Obj) -> Result<Value> {
    Ok(parts(o)?.0)
}
