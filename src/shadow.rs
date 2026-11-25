//! `ShadowRealm`: a realm (see `realm`) of its own, which code of the
//! realm making it reaches only through `evaluate` and `importValue`, and
//! which passes nothing back but primitives and callables: a callable
//! crosses as a wrapped function, which wraps what it is passed and what
//! it returns the same way (and turns what is thrown into a `TypeError`).

use crate::brand::{Args, Fresh, Scope};
use crate::object::{Class, Obj, Object, as_object, object_value};
use crate::promise::{native_closure, slot};
use crate::props::{Desc, is_callable, is_object};
use crate::raw::{Live, ObjectPtr};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, is_string, string_units};

/// `Vm::shadow_roots`, a realm's.
enum Root {
    Prototype,
    /// The template of the wrapped functions made for this realm (slot:
    /// the target).
    Wrapped,
    /// `importValue`'s reactions (slots: the module, the export's name,
    /// resolve, reject; reject alone).
    Imported,
    ImportFailed,
}

fn root(vm: &Vm, r: Root) -> Obj {
    vm.shadow_roots[r as usize]
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn hidden_key(vm: &mut Vm) -> Key {
    Key::Name(vm.intern_name("\u{FDD0}ShadowRealm"))
}

pub(crate) fn install(vm: &mut Vm) {
    let any = crate::jit::Ty::Any;
    let g = vm.global_obj();
    let op = object_value(vm.object_prototype);
    let proto = vm.new_plain(op);
    let c = vm.define_native(g, "ShadowRealm", shadow_realm_ctor, 0, any, true);
    crate::builtins::link_constructor(vm, c, proto);
    vm.define_natives(proto, &[("evaluate", evaluate as NativeFn, 1), ("importValue", import_value, 2)]);
    crate::symbol::to_string_tag(vm, proto, "ShadowRealm");
    let wrapped = vm.native_function("", wrapped_call, 0, any, false);
    let imported = vm.native_function("", imported, 1, any, false);
    let failed = vm.native_function("", import_failed, 1, any, false);
    vm.shadow_roots = vec![proto, wrapped, imported, failed];
}

/// `new ShadowRealm()`: a new realm, its global object the host's.
fn shadow_realm_ctor(vm: &mut Vm, _: Value, _: Args<'_>) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor ShadowRealm requires 'new'"));
    }
    let callee = vm.callee_obj();
    let default = object_value(root(vm, Root::Prototype).ptr());
    let proto = vm.take_new_target(callee, default)?;
    let o = vm.new_plain(proto);
    let r = crate::realm::create(vm);
    let k = hidden_key(vm);
    let d = Desc::data(Value::from_i32(r as i32), false, false, false);
    vm.define_own_property(o, k, &d)?;
    Ok(object_value(o.ptr()))
}

/// ValidateShadowRealmObject: the realm of ShadowRealm `v`.
fn realm_of(vm: &mut Vm, v: Value, what: &str) -> Result<u32> {
    if is_object(v)
        && Obj::of(v).class() == Class::Plain
        && let k = hidden_key(vm)
        && let Some(d) = vm.own_property(Obj::of(v), k)
        && let Some(r) = d.value.and_then(|r| r.as_number())
    {
        return Ok(r as u32);
    }
    Err(Error::type_error(format!("ShadowRealm.prototype.{what} called on an incompatible receiver")))
}

/// `ShadowRealm.prototype.evaluate(sourceText)`: the script run in the
/// realm, its result wrapped.
fn evaluate(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let r = realm_of(vm, this, "evaluate")?;
    let Some(units) = string_units(arg(&a, 0)) else {
        return Err(Error::type_error("ShadowRealm.prototype.evaluate requires a string"));
    };
    let src = crate::js::lex::Source::from_units(units);
    // An indirect `eval`'s code (its lexical declarations its own). A
    // syntax error, any (the functions' bodies parsed too), is the
    // caller's.
    let syntax = |e: String| Error(format!("SyntaxError: {e}"), None, 0);
    crate::js::check_source(&src).map_err(|e| syntax(e.display_source(&src)))?;
    let module = crate::js::compile_indirect_eval_source(&src).map_err(syntax)?;
    let out = crate::realm::run_in(vm, r, |vm| {
        vm.global_declarations(&module, true)?;
        let code = vm.load(module);
        crate::interp::run_script_at(vm, code)
    });
    match out {
        Ok(v) => wrap(vm, v),
        Err(e) if e.0.starts_with("unsupported") => Err(e),
        Err(e) => Err(Error::type_error(format!("ShadowRealm evaluation threw: {}", thrown_message(vm, &e)))),
    }
}

/// What `e` threw, for the `TypeError` a crossing turns it into, found
/// without running code (no `toString`, no getter): an error object's
/// `name` and `message` if data properties (its own, or its prototype's
/// `name`), else the error's own message. (The caller's frames are live:
/// `Vm::uncaught_message` would run over them.)
fn thrown_message(vm: &mut Vm, e: &Error) -> String {
    let Some(v) = e.1.filter(|&v| is_object(v)) else {
        return e.0.clone();
    };
    let o = Obj::of(v);
    if !crate::object::is_error(o.ptr()) {
        return e.0.clone();
    }
    let data = |vm: &mut Vm, o: *mut Object, name| match vm.own_property(unsafe { Obj::from_ptr(o) }, Key::Name(name)) {
        Some(Desc { value: Some(s), .. }) => string_units(s).map(String::from_utf16_lossy),
        _ => None,
    };
    let (a_name, a_message) = (vm.a_name, vm.intern_name("message"));
    let proto = o.shape().live().proto;
    let name = data(vm, o.ptr(), a_name)
        .or_else(|| is_object(proto).then(|| data(vm, as_object(proto), a_name)).flatten())
        .unwrap_or_else(|| "Error".into());
    match data(vm, o.ptr(), a_message) {
        Some(m) if !m.is_empty() => format!("{name}: {m}"),
        _ => name,
    }
}

/// GetWrappedValue(current realm, `v`): a primitive as it is, a callable
/// wrapped; anything else a `TypeError`.
fn wrap(vm: &mut Vm, v: Value) -> Result<Value> {
    if !is_object(v) {
        return Ok(v);
    }
    if !is_callable(v) {
        return Err(Error::type_error("ShadowRealm values crossing must be primitives or callables"));
    }
    let template = root(vm, Root::Wrapped);
    let f = native_closure(vm, template, &[v]);
    // (The wrapper, and its target, held across the target's `length`
    // and `name` getters and traps: rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([f, v]);
    copy_name_and_length(vm, f, v)
        .map_err(|e| Error::type_error(format!("wrapping a function: {}", thrown_message(vm, &e))))?;
    Ok(f)
}

/// [`wrap`] in realm `r`.
fn wrap_in(vm: &mut Vm, r: u32, v: Value) -> Result<Value> {
    if !is_object(v) {
        return Ok(v);
    }
    crate::realm::run_in(vm, r, |vm| wrap(vm, v))
}

/// CopyNameAndLength(f, target): `f`'s own `length` and `name` from the
/// target's.
fn copy_name_and_length(vm: &mut Vm, f: Value, target: Value) -> Result<()> {
    let (a_length, a_name) = (vm.a_length, vm.a_name);
    let mut len = 0.0;
    if crate::proxy::get_own_property(vm, Obj::of(target), Key::Name(a_length))?.is_some() {
        let l = vm.get_atom_b(target, a_length)?;
        if let Some(n) = l.peek().as_number() {
            len = if n == f64::INFINITY {
                n
            } else if n == f64::NEG_INFINITY || n.is_nan() {
                0.0
            } else {
                n.trunc().max(0.0)
            };
        }
    }
    let d = Desc::data(Value::number(len), false, false, true);
    vm.define_own_property(Obj::of(f), Key::Name(a_length), &d)?;
    let name = vm.get_atom_b(target, a_name)?;
    let name = if is_string(name.peek()) { name.peek() } else { vm.new_string(&[]) };
    let d = Desc::data(name, false, false, true);
    vm.define_own_property(Obj::of(f), Key::Name(a_name), &d)?;
    Ok(())
}

/// A wrapped function's call (in its own realm): the target called in
/// its realm with the arguments and `this` wrapped for it, the result
/// wrapped back. Whatever is thrown is a `TypeError`.
fn wrapped_call(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let target = slot(vm.callee_obj(), 0);
    let r = (|| {
        let target_realm = crate::realm::function_realm(vm, target)?;
        vm.handle_scope(|vm| {
            let mut args = Vec::with_capacity(a.len());
            for &v in a {
                let w = wrap_in(vm, target_realm, v)?;
                vm.handles.push(w);
                args.push(w);
            }
            let this = wrap_in(vm, target_realm, this)?;
            let sc_v = Scope::new(vm);
            let v = vm.call_b(target, this, &args)?.root(&sc_v);
            wrap(vm, v.peek())
        })
    })();
    match r {
        Err(e) if e.0.starts_with("unsupported") => Err(e),
        Err(e) => {
            let m = thrown_message(vm, &e);
            Err(Error::type_error(format!("a wrapped function threw: {m}")))
        }
        ok => ok,
    }
}

/// `ShadowRealm.prototype.importValue(specifier, exportName)`: a promise
/// of the module's export, imported in the realm, wrapped.
fn import_value(vm: &mut Vm, this: Value, a: Args<'_>) -> Result<Value> {
    let r = realm_of(vm, this, "importValue")?;
    let sc = Scope::new(vm);
    let spec = vm.to_string_b(arg(&a, 0))?.root(&sc);
    let name = arg(&a, 1);
    if !is_string(name) {
        return Err(Error::type_error("ShadowRealm.prototype.importValue requires a string export name"));
    }
    let (p, resolve, reject) = crate::promise::new_with_resolvers(vm);
    // (The promise and its functions held across the module's evaluation:
    // rooted.)
    let held = Scope::new(vm);
    let _rooted = held.roots([p, resolve, reject]);
    let loaded = crate::realm::run_in(vm, r, |vm| crate::module::import_now(vm, spec.peek()));
    match loaded {
        Ok((m, done)) => {
            let ok = native_closure(vm, root(vm, Root::Imported), &[Value::from_i32(m as i32), name, resolve, reject]);
            let bad = native_closure(vm, root(vm, Root::ImportFailed), &[reject]);
            crate::promise::await_then(vm, done, ok, bad)?;
        }
        Err(e) if e.0.starts_with("unsupported") => return Err(e),
        Err(e) => {
            let m = thrown_message(vm, &e);
            let v = crate::builtins::caught(vm, &Error::type_error(format!("importValue failed: {m}")));
            vm.call_b(reject, Value::UNDEFINED, &[v])?.take();
        }
    }
    Ok(p)
}

/// `importValue`'s module evaluated: its export, wrapped (none: a
/// `TypeError`).
fn imported(vm: &mut Vm, _: Value, _: Args<'_>) -> Result<Value> {
    let me = vm.callee_obj();
    let (m, name, resolve, reject) = (slot(me, 0), slot(me, 1), slot(me, 2), slot(me, 3));
    let r = (|| {
        let ns = crate::module::namespace(vm, m.as_number().unwrap_or(0.0) as usize)?;
        let k = vm.property_key(name);
        if !crate::proxy::generic_has(vm, ns, k)? {
            let n = String::from_utf16_lossy(string_units(name).unwrap_or(&[]));
            return Err(Error::type_error(format!("The module has no export '{n}'")));
        }
        let v = crate::proxy::generic_get(vm, ns, k, ns)?;
        wrap(vm, v)
    })();
    match r {
        Ok(v) => vm.call_b(resolve, Value::UNDEFINED, &[v]).map(Fresh::take),
        Err(e) if e.0.starts_with("unsupported") => Err(e),
        Err(e) => {
            let m = thrown_message(vm, &e);
            let v = crate::builtins::caught(vm, &Error::type_error(m));
            vm.call_b(reject, Value::UNDEFINED, &[v]).map(Fresh::take)
        }
    }
}

/// `importValue`'s module failed: a `TypeError`.
fn import_failed(vm: &mut Vm, _: Value, a: Args<'_>) -> Result<Value> {
    let reject = slot(vm.callee_obj(), 0);
    let e = Error::thrown(arg(&a, 0));
    let m = thrown_message(vm, &e);
    let v = crate::builtins::caught(vm, &Error::type_error(format!("importValue failed: {m}")));
    vm.call_b(reject, Value::UNDEFINED, &[v]).map(Fresh::take)
}
