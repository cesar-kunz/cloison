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

use crate::brand::{Args, Fresh, Scope};
use crate::interp::to_boolean;
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
#[inline(always)]
fn trap<'v>(vm: &'v mut Vm, h: Value, name: &str) -> Result<Fresh<'v, Option<Value>>> {
    trap_raw(vm, h, name).map(Fresh::new)
}

fn trap_raw(vm: &mut Vm, h: Value, name: &str) -> Result<Option<Value>> {
    if crate::stack::low() {
        return Err(Error::range_error("Maximum call stack size exceeded"));
    }
    let a = vm.intern_name(name);
    let m = vm.get_atom_b(h, a)?;
    if m.peek().is_undefined() || m.peek().is_null() {
        return Ok(None);
    }
    if !is_callable(m.peek()) {
        return Err(Error::type_error(format!("The proxy's '{name}' trap is not a function")));
    }
    Ok(Some(m.peek()))
}

/// The values of a descriptor `d` (its value, getter and setter), to root.
pub(crate) fn desc_values(d: Option<&Desc>) -> [Value; 3] {
    let v = |v: Option<Value>| v.unwrap_or(Value::UNDEFINED);
    d.map_or([Value::UNDEFINED; 3], |d| [v(d.value), v(d.get), v(d.set)])
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
#[inline(always)]
pub(crate) fn create(vm: &mut Vm, target: Value, handler: Value) -> Result<Fresh<'_>> {
    create_raw(vm, target, handler).map(Fresh::new)
}

fn create_raw(vm: &mut Vm, target: Value, handler: Value) -> Result<Value> {
    if !is_object(target) || !is_object(handler) {
        return Err(Error::type_error("Cannot create proxy with a non-object as target or handler"));
    }
    vm.proxies_exist = true;
    let callable = is_callable(target);
    let class = if callable { Class::Native } else { Class::Proxy };
    // (Its target, its handler and its private elements' holder.)
    let full = vm.root_shape(class, Value::NULL, 3);
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
    set_slot(o, 2, Value::UNDEFINED);
    Ok(object_value(o.ptr()))
}

/// The object holding proxy `p`'s private elements (its
/// [[PrivateElements]], 7.3.26-28), made if `make` and none yet. They are
/// the proxy's own, its traps never see them (no internal method of the
/// proxy runs for them), and they outlive a revoke.
pub(crate) fn private_holder(vm: &mut Vm, p: Obj, make: bool) -> Option<Obj> {
    let h = slot(p, 2);
    if h.is_cell() {
        return Some(Obj::of(h));
    }
    if !make {
        return None;
    }
    let h = vm.mu().new_plain(Value::NULL);
    set_slot(p, 2, object_value(h.obj().ptr()));
    Some(h.obj())
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// `new Proxy(target, handler)`.
fn proxy_ctor(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor Proxy requires 'new'"));
    }
    create(vm, arg(&a, 0), arg(&a, 1)).map(Fresh::take)
}

/// `Proxy.revocable(target, handler)`: `{ proxy, revoke }`.
fn proxy_revocable(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    let sc_p = Scope::new(vm);
    let p = create(vm, arg(&a, 0), arg(&a, 1))?.root(&sc_p);
    let full = vm.root_shape(Class::Native, object_value(vm.function_prototype), 1);
    let bare = vm.root_shape(Class::Native, object_value(vm.function_prototype), 0);
    let r = vm.heap.alloc_object(full.live());
    unsafe {
        crate::object::set_shape(r, bare);
        (*r).code = vm.proxy_codes[2];
    }
    set_slot(unsafe { Obj::from_ptr(r) }, 0, p.peek());
    let out = {
        let arg = object_value(vm.object_prototype);
        vm.mu().new_plain(arg).v()
    };
    let (a_proxy, a_revoke) = (vm.intern_name("proxy"), vm.intern_name("revoke"));
    vm.create_data_property(out, Key::Name(a_proxy), p.peek())?;
    vm.create_data_property(out, Key::Name(a_revoke), object_value(r))?;
    Ok(out)
}

/// A revocable proxy's `revoke()`: its proxy's target and handler gone.
fn revoke(vm: &mut Vm, _: Value, _: Args<'_>) -> Result<Value> {
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
fn trampoline(vm: &mut Vm, this: Value, args: Args<'_>) -> Result<Value> {
    let constructing = vm.is_construct();
    let p = vm.callee_obj();
    let nt = std::mem::replace(&mut vm.new_target, Value::EMPTY);
    let (t, h) = parts(p)?;
    // (The target and handler, which a trap's lookup may revoke,
    // `new.target`, taken from the VM, and the arguments' list rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([t, h, nt]);
    let sc_list = Scope::new(vm);
    let list = crate::array::new_array_of(vm, &args)?.root(&sc_list);
    {
        let held = Scope::new(vm);
        let _held = held.roots([list.peek()]);
        trampoline_with(vm, constructing, p, t, h, nt, this, &args, list.peek())
    }
}

/// [`trampoline`] with its arguments' array `list`.
#[allow(clippy::too_many_arguments)]
fn trampoline_with(
    vm: &mut Vm,
    constructing: bool,
    p: Obj,
    t: Value,
    h: Value,
    nt: Value,
    this: Value,
    args: &[Value],
    list: Value,
) -> Result<Value> {
    if constructing {
        let nt = if nt == Value::EMPTY { object_value(p.ptr()) } else { nt };
        // take: given at once to the call.
        let Some(f) = trap(vm, h, "construct")?.take() else {
            return crate::intrinsics::construct_with(vm, t, args, nt);
        };
        let r = vm.call_b(f, h, &[t, list, nt])?;
        if !is_object(r.peek()) {
            return Err(Error::type_error("The proxy's 'construct' trap returned a non-object"));
        }
        return Ok(r.peek());
    }
    // take: given at once to the call.
    match trap(vm, h, "apply")?.take() {
        Some(f) => vm.call_b(f, h, &[t, this, list]).map(Fresh::take),
        None => vm.call_b(t, this, args).map(Fresh::take),
    }
}

// ---- The internal methods of proxies ----

/// A proxy's `[[Get]]` by its handler `h`'s `get` trap `f`, its target
/// `t` (one without the trap forwards in `generic_get`'s loop).
#[inline(always)]
fn get(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key, receiver: Value) -> Result<Fresh<'_>> {
    get_raw(vm, t, h, f, key, receiver).map(Fresh::new)
}

fn get_raw(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key, receiver: Value) -> Result<Value> {
    let sc = Scope::new(vm);
    let _rooted = sc.roots([t, h, f]);
    let k = trap_key(vm, key);
    // (Read after the target's traps.)
    let v = vm.call_b(f, h, &[t, k, receiver])?.root(&sc).v();
    get_checked(vm, t, key, v).map(Fresh::take)
}

/// [`get`]'s check of the trap's result `v` against the target's property.
#[inline(always)]
fn get_checked(vm: &mut Vm, t: Value, key: Key, v: Value) -> Result<Fresh<'_>> {
    get_checked_raw(vm, t, key, v).map(Fresh::new)
}

fn get_checked_raw(vm: &mut Vm, t: Value, key: Key, v: Value) -> Result<Value> {
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
    {
        let held = Scope::new(vm);
        let _held = held.roots([t, h, f, v]);
        set_rooted(vm, t, h, f, key, v, receiver)
    }
}

/// [`set`], its values rooted.
fn set_rooted(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key, v: Value, receiver: Value) -> Result<bool> {
    let k = trap_key(vm, key);
    if !to_boolean(vm.call_b(f, h, &[t, k, v, receiver])?.take()) {
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
    {
        let held = Scope::new(vm);
        let _held = held.roots([t, h, f]);
        has_rooted(vm, t, h, f, key)
    }
}

/// [`has`], its values rooted.
fn has_rooted(vm: &mut Vm, t: Value, h: Value, f: Value, key: Key) -> Result<bool> {
    let k = trap_key(vm, key);
    if to_boolean(vm.call_b(f, h, &[t, k])?.take()) {
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
    // (A getter of the trap may revoke the proxy: its parts rooted.)
    Ok((
        t,
        h,
        {
            let held = Scope::new(vm);
            let _held = held.roots([t, h]);
            trap(vm, h, name)
        }?
        .take(),
    ))
}

/// Proxy `o`'s target, handler and trap `name` (if it has one), the
/// parts rooted while the trap is looked up: a getter of it may revoke the
/// proxy, its target then held here only. Without one, the caller forwards
/// to the target from its own small frame, which a chain of proxies does
/// as deep as it is; with one, it runs the trap in [`trapped`]'s.
#[inline(always)]
fn trap_of(vm: &mut Vm, o: Obj, name: &str) -> Result<(Value, Value, Option<Value>)> {
    let (t, h) = parts(o)?;
    Ok((
        t,
        h,
        {
            let held = Scope::new(vm);
            let _held = held.roots([t, h]);
            trap(vm, h, name)
        }?
        .take(),
    ))
}

/// Run a proxy's trap `f` (`run`, given its target `t` and handler `h`),
/// all three rooted: a trap may revoke the proxy, and `f` may be a
/// getter's only. Out of line, so that its frame is not part of the frames
/// of a chain of trapless proxies forwarding to their targets.
#[inline(never)]
fn trapped<T>(
    vm: &mut Vm,
    t: Value,
    h: Value,
    f: Value,
    run: impl FnOnce(&mut Vm, Value, Value, Value) -> Result<T>,
) -> Result<T> {
    {
        let held = Scope::new(vm);
        let _held = held.roots([t, h, f]);
        run(vm, t, h, f)
    }
}

/// `o.[[Get]](key, receiver)`.
pub(crate) fn generic_get(vm: &mut Vm, o: Value, key: Key, receiver: Value) -> Result<Value> {
    // (`receiver`, maybe a Rust caller's only, held across a trap's
    // getter: rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([receiver]);
    let mut cur = Obj::of(o);
    let mut hops = 0;
    loop {
        if is_proxy(vm, cur) {
            match forward(vm, cur, "get", &mut hops)? {
                (t, h, Some(f)) => return get(vm, t, h, f, key, receiver).map(Fresh::take),
                (t, _, None) => {
                    cur = Obj::of(t);
                    continue;
                }
            }
        }
        if let Some(d) = vm.own_property(cur, key) {
            if d.is_accessor() {
                return match d.get.filter(|g| !g.is_undefined()) {
                    Some(g) => vm.call_b(g, receiver, &[]).map(Fresh::take),
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
    // (`v` and `receiver`, maybe a Rust caller's only, held across a
    // trap's getter and the receiver's own traps: rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([v, receiver]);
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
                vm.call_b(s, receiver, &[v])?.take();
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
    let (t, h, f) = trap_of(vm, o, "deleteProperty")?;
    let Some(f) = f else {
        return delete(vm, Obj::of(t), key);
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        let k = trap_key(vm, key);
        if !to_boolean(vm.call_b(f, h, &[t, k])?.take()) {
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
    })
}

/// `o.[[GetOwnProperty]](key)`.
pub(crate) fn get_own_property(vm: &mut Vm, o: Obj, key: Key) -> Result<Option<Desc>> {
    if !is_proxy(vm, o) {
        return Ok(vm.own_property(o, key));
    }
    let (t, h, f) = trap_of(vm, o, "getOwnPropertyDescriptor")?;
    let Some(f) = f else {
        return get_own_property(vm, Obj::of(t), key);
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        let k = trap_key(vm, key);
        // take: rooted below; the check between doesn't collect.
        let r = vm.call_b(f, h, &[t, k])?.take();
        if !r.is_undefined() && !is_object(r) {
            return Err(Error::type_error(
                "The proxy's 'getOwnPropertyDescriptor' trap returned neither an object nor undefined",
            ));
        }
        // (`r`, then the target's property's values, compared after the
        // target's traps and `r`'s getters, rooted.)
        let target = {
            let held = Scope::new(vm);
            let _held = held.roots([r]);
            get_own_property(vm, Obj::of(t), key)
        }?;
        let [a, b, c] = desc_values(target.as_ref());
        {
            let held = Scope::new(vm);
            let _held = held.roots([r, a, b, c]);
            own_property_checked(vm, t, r, target).map(Fresh::take)
        }
    })
}

/// [`get_own_property`]'s check of its trap's result `r` against the
/// target's property `target`: the descriptor it stands for.
#[inline(always)]
fn own_property_checked(vm: &mut Vm, t: Value, r: Value, target: Option<Desc>) -> Result<Fresh<'_, Option<Desc>>> {
    own_property_checked_raw(vm, t, r, target).map(Fresh::new)
}

fn own_property_checked_raw(vm: &mut Vm, t: Value, r: Value, target: Option<Desc>) -> Result<Option<Desc>> {
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

/// [`proxy_define`] forwarding to its target `t`, not a proxy: `t`
/// rooted, as the define may run JS (an array's `length` converted, a
/// typed array's element) that revokes the proxy, or the trap's getter
/// did, `t` then held here only. Out of line: the forwarding frame a
/// chain of proxies repeats stays small.
#[inline(never)]
fn define_on_target(vm: &mut Vm, t: Value, key: Key, d: &Desc) -> Result<bool> {
    let held = Scope::new(vm);
    let [t] = held.roots([t]);
    define_property(vm, t.obj(), key, d)
}

/// A proxy's `[[DefineOwnProperty]]`.
pub(crate) fn proxy_define(vm: &mut Vm, o: Obj, key: Key, d: &Desc) -> Result<bool> {
    let (t, h, f) = trap_of(vm, o, "defineProperty")?;
    let Some(f) = f else {
        if !is_proxy(vm, Obj::of(t)) {
            return define_on_target(vm, t, key, d);
        }
        return proxy_define(vm, Obj::of(t), key, d);
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        // (`d`'s values, maybe a Rust caller's only, rooted across the
        // traps.)
        let [a, b, c] = desc_values(Some(d));
        let Some((target, extensible)) = {
            let held = Scope::new(vm);
            let _held = held.roots([a, b, c]);
            define_trapped(vm, t, h, f, key, d)
        }?
        else {
            return Ok(false);
        };
        let config_false = d.configurable == Some(false);
        match target {
            None if !extensible => Err(broken("defineProperty", "added a property to a non-extensible target")),
            None if config_false => {
                Err(broken("defineProperty", "defined a non-configurable property the target lacks"))
            }
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
                if td.is_data()
                    && td.configurable == Some(false)
                    && td.writable == Some(true)
                    && d.writable == Some(false)
                {
                    return Err(broken("defineProperty", "made a writable, non-configurable property non-writable"));
                }
                Ok(true)
            }
        }
    })
}

/// [`proxy_define`]'s call of its trap `f`: `None` if it says no, else
/// the target's property and whether the target is extensible.
fn define_trapped(
    vm: &mut Vm,
    t: Value,
    h: Value,
    f: Value,
    key: Key,
    d: &Desc,
) -> Result<Option<(Option<Desc>, bool)>> {
    let k = trap_key(vm, key);
    let dv = {
        let held = Scope::new(vm);
        let _held = held.roots([k]);
        from_partial_descriptor(vm, d)
    };
    if !to_boolean(vm.call_b(f, h, &[t, k, dv])?.take()) {
        return Ok(None);
    }
    let target = get_own_property(vm, Obj::of(t), key)?;
    // (The target's property's values, compared after its trap, rooted.)
    let [a, b, c] = desc_values(target.as_ref());
    let extensible = {
        let held = Scope::new(vm);
        let _held = held.roots([a, b, c]);
        is_extensible(vm, Obj::of(t))
    }?;
    Ok(Some((target, extensible)))
}

/// A descriptor object of just the fields `d` has.
fn from_partial_descriptor(vm: &mut Vm, d: &Desc) -> Value {
    let o = {
        let arg = object_value(vm.object_prototype);
        vm.mu().new_plain(arg).v()
    };
    // (Rooted as its properties are added.)
    {
        let held = Scope::new(vm);
        let _held = held.roots([o]);
        fill_descriptor(vm, o, d)
    };
    o
}

/// [`from_partial_descriptor`]'s fields, defined on `o`.
fn fill_descriptor(vm: &mut Vm, o: Value, d: &Desc) {
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
    let (t, h, f) = trap_of(vm, o, "ownKeys")?;
    let Some(f) = f else {
        return own_keys(vm, Obj::of(t));
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        // take: rooted by the closure below; the check between doesn't collect.
        let r = vm.call_b(f, h, &[t])?.take();
        if !is_object(r) {
            return Err(Error::type_error("The proxy's 'ownKeys' trap returned a non-object"));
        }
        // (`r` rooted across its getters.)
        let keys = vm.rooted(&[r], |vm| {
            let n = vm.length_of(r)?;
            let mut keys = Vec::new();
            let mut i = 0.0;
            while i < n {
                // take: checked, then given at once to to_property_key.
                let k = vm.get_keyed_b(r, Value::number(i))?.take();
                if !crate::vm::is_string(k) && !crate::symbol::is_symbol(k) {
                    return Err(Error::type_error(
                        "The proxy's 'ownKeys' trap gave a key neither a string nor a symbol",
                    ));
                }
                keys.push(crate::intrinsics::to_property_key(vm, k)?);
                i += 1.0;
            }
            Ok(keys)
        })?;
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
    })
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
    let (t, h, f) = trap_of(vm, o, "getPrototypeOf")?;
    let Some(f) = f else {
        return get_prototype_of(vm, Obj::of(t));
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        // take: rooted by the closure below; the check between doesn't collect.
        let r = vm.call_b(f, h, &[t])?.take();
        if !is_object(r) && !r.is_null() {
            return Err(Error::type_error("The proxy's 'getPrototypeOf' trap returned neither an object nor null"));
        }
        // (`r` rooted across the target's traps.)
        vm.rooted(&[r], |vm| {
            if !is_extensible(vm, Obj::of(t))? && !same_value(r, get_prototype_of(vm, Obj::of(t))?) {
                return Err(broken("getPrototypeOf", "reported a non-extensible target's prototype otherwise"));
            }
            Ok(r)
        })
    })
}

/// A step of a walk up `o`'s prototype chain (OrdinaryHasInstance,
/// `isPrototypeOf`, for-in): `o.[[GetPrototypeOf]]()`, a proxy counted in
/// `hops`. Past [`MAX_FORWARDS`] proxies a RangeError, as a lookup's: a
/// cycle through a proxy (10.1.2.1 step 7.b.i) has no end, and nothing
/// else bounds the walk (#336).
#[inline(always)]
pub(crate) fn walk_prototype<'v>(vm: &'v mut Vm, o: Obj, hops: &mut u32) -> Result<Fresh<'v>> {
    walk_prototype_raw(vm, o, hops).map(Fresh::new)
}

fn walk_prototype_raw(vm: &mut Vm, o: Obj, hops: &mut u32) -> Result<Value> {
    if !is_proxy(vm, o) {
        return Ok(o.shape().live().proto);
    }
    *hops += 1;
    if *hops > MAX_FORWARDS {
        return Err(Error::range_error("Maximum call stack size exceeded"));
    }
    get_prototype_of(vm, o)
}

/// `o.[[SetPrototypeOf]](proto)`: whether it was set.
pub(crate) fn set_prototype_of(vm: &mut Vm, o: Obj, proto: Value) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(vm.set_prototype_of(o, proto));
    }
    let (t, h, f) = trap_of(vm, o, "setPrototypeOf")?;
    let Some(f) = f else {
        return set_prototype_of(vm, Obj::of(t), proto);
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        if !to_boolean(vm.call_b(f, h, &[t, proto])?.take()) {
            return Ok(false);
        }
        if !is_extensible(vm, Obj::of(t))? && !same_value(proto, get_prototype_of(vm, Obj::of(t))?) {
            return Err(broken("setPrototypeOf", "changed a non-extensible target's prototype"));
        }
        Ok(true)
    })
}

/// `o.[[IsExtensible]]()`.
pub(crate) fn is_extensible(vm: &mut Vm, o: Obj) -> Result<bool> {
    if !is_proxy(vm, o) {
        return Ok(Vm::is_extensible(o));
    }
    let (t, h, f) = trap_of(vm, o, "isExtensible")?;
    let Some(f) = f else {
        return is_extensible(vm, Obj::of(t));
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        let r = to_boolean(vm.call_b(f, h, &[t])?.take());
        if r != is_extensible(vm, Obj::of(t))? {
            return Err(broken("isExtensible", "reported the target's extensibility otherwise"));
        }
        Ok(r)
    })
}

/// `o.[[PreventExtensions]]()`: whether it now isn't extensible.
pub(crate) fn prevent_extensions(vm: &mut Vm, o: Obj) -> Result<bool> {
    if !is_proxy(vm, o) {
        vm.prevent_extensions(o);
        return Ok(true);
    }
    let (t, h, f) = trap_of(vm, o, "preventExtensions")?;
    let Some(f) = f else {
        return prevent_extensions(vm, Obj::of(t));
    };
    trapped(vm, t, h, f, |vm, t, h, f| {
        let r = to_boolean(vm.call_b(f, h, &[t])?.take());
        if r && is_extensible(vm, Obj::of(t))? {
            return Err(broken("preventExtensions", "reported an extensible target as not"));
        }
        Ok(r)
    })
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
    // (The object the walk is at, a trap's result maybe, rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([object_value(o.ptr())]);
    let slot = vm.handles.len() - 1;
    let mut seen: HashSet<Key> = HashSet::default();
    let mut out = Vec::new();
    let mut cur = o;
    let mut hops = 0;
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
        let p = walk_prototype(vm, cur, &mut hops)?;
        if !p.peek().is_cell() {
            return Ok(out);
        }
        cur = Obj::of(p.peek());
        vm.handles[slot] = p.peek();
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
