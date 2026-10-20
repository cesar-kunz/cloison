//! ECMAScript modules (ECMA-262 §16.2): a module graph loaded from files,
//! linked, and evaluated (top-level `await` too); namespace objects;
//! `import()` and `import.meta`; JSON, text and bytes modules (`with {
//! type: "json" }`, `"text"`, `"bytes"`).
//!
//! A module compiles to a script whose value is the module's function
//! (see `parse::Parser::module_body`): a generator whose call (linking)
//! fills the module's scope object, made here, with its bindings (its
//! functions made, its other variables uninitialized) and stops, and
//! whose resumption (evaluation) runs the module's body; an async
//! module's runs as an async function's coroutine. An import reads the
//! exporter's scope object, which the importing module's function gets
//! as a parameter (see `ast::Import`), so imports are live, and see their
//! exporter's uninitialized variables as such.
//!
//! The records live in `Vm::modules` ([`Modules`]), by path (and type: a
//! JSON module's file is another module than its JavaScript one), each
//! loaded once. Linking and evaluation follow the spec's algorithms
//! (§16.2.1.5), a module's index its identity.
//!
//! A namespace object is a proxy (see `proxy`) of a frozen-shaped target
//! (each export a non-configurable property, `Symbol.toStringTag`) whose
//! handler's traps (natives reading the module's index in a hidden slot:
//! see `promise::native_closure`) read the bindings live, as §10.4.6's
//! internal methods do.

use crate::interp::call_function;
use crate::js::ast::ModuleInfo;
use crate::object::{Object, as_object, object_value};
use crate::promise::{native_closure, slot};
use crate::props::{Desc, same_value};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, string_units};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Status {
    /// Loaded, not linked (or its linking failed).
    New,
    Linking,
    Linked,
    Evaluating,
    EvaluatingAsync,
    Evaluated,
}

/// What a module is, by its request's `type`.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Kind {
    Js,
    Json,
    /// Its file as a string (UTF-8).
    Text,
    /// Its file's bytes, a `Uint8Array` on an immutable `ArrayBuffer`.
    Bytes,
}

/// A module record (a Cyclic Module Record's fields).
pub(crate) struct Record {
    /// Its file, canonical.
    path: String,
    /// What it is: a JSON, text or bytes one's only export, `default`, is
    /// its value.
    kind: Kind,
    source: String,
    info: ModuleInfo,
    /// Each request's module (see `ModuleInfo::requests`).
    requested: Vec<usize>,
    status: Status,
    /// Its scope object: its variables, which its importers read.
    env: Value,
    namespace: Value,
    /// Its function's generator (see the module docs), once linked.
    generator: Value,
    /// What its evaluation threw.
    error: Option<Value>,
    dfs_index: usize,
    dfs_ancestor: usize,
    cycle_root: usize,
    /// [[AsyncEvaluation]]: when it was marked so (its order), while it is.
    async_order: Option<u64>,
    /// Ever marked [[AsyncEvaluation]] (an evaluated module stays async).
    was_async: bool,
    pending_deps: usize,
    async_parents: Vec<usize>,
    /// [[TopLevelCapability]]: a promise and its resolving functions.
    capability: Option<(Value, Value, Value)>,
    /// `import.meta`, once read.
    meta: Value,
    /// Its namespace's names, sorted, each with its binding: a module and
    /// its variable, or a module's namespace (no variable).
    exports: Vec<(Vec<u16>, usize, Option<String>)>,
}

impl Record {
    fn values(&self) -> impl Iterator<Item = Value> + '_ {
        let cap = self.capability.iter().flat_map(|&(p, f, r)| [p, f, r]);
        [self.env, self.namespace, self.generator, self.meta].into_iter().chain(self.error).chain(cap)
    }
}

/// The VM's modules.
#[derive(Default)]
pub(crate) struct Modules {
    records: Vec<Record>,
    /// Each module's index, by its path and whether it is JSON.
    by_path: std::collections::HashMap<(String, Kind), usize>,
    /// The natives the closures here are made of (see [`Template`]).
    templates: Vec<*mut Object>,
    /// The next [[AsyncEvaluation]] order.
    async_order: u64,
    /// The script's file (a script's `import()`s resolve from it).
    pub(crate) script_path: Option<String>,
}

impl Modules {
    /// The values the records hold (GC roots).
    pub(crate) fn roots(&self) -> Vec<Value> {
        let mut v: Vec<Value> = self.records.iter().flat_map(|r| r.values()).filter(|v| v.is_cell()).collect();
        v.extend(self.templates.iter().map(|&t| object_value(t)));
        v
    }
}

/// The natives of closures here, indices into `Modules::templates`.
#[derive(Clone, Copy)]
enum Template {
    Get,
    Set,
    Has,
    Delete,
    Define,
    GetOwn,
    OwnKeys,
    /// An async module's evaluation's reactions (slot: the module).
    Fulfilled,
    Rejected,
    /// `import()`'s job (slots: the specifier, its type, the referrer,
    /// resolve, reject) and its namespace's (slots: the module, resolve).
    ImportJob,
    ImportDone,
}

fn template(vm: &mut Vm, t: Template) -> *mut Object {
    if vm.modules.templates.is_empty() {
        let any = crate::jit::Ty::Any;
        let fs: [(NativeFn, u32); 11] = [
            (ns_get, 3),
            (ns_set, 4),
            (ns_has, 2),
            (ns_delete, 2),
            (ns_define, 3),
            (ns_get_own, 2),
            (ns_own_keys, 1),
            (async_fulfilled, 1),
            (async_rejected, 1),
            (import_job, 0),
            (import_done, 0),
        ];
        let names = ["get", "set", "has", "deleteProperty", "defineProperty", "getOwnPropertyDescriptor", "ownKeys", "", "", "", ""];
        for ((f, n), name) in fs.into_iter().zip(names) {
            let o = vm.native_function(name, f, n, any, false);
            vm.modules.templates.push(o);
        }
    }
    vm.modules.templates[t as usize]
}

fn closure(vm: &mut Vm, t: Template, data: &[Value]) -> Value {
    let t = template(vm, t);
    native_closure(vm, t, data)
}

fn rec(vm: &Vm, m: usize) -> &Record {
    &vm.modules.records[m]
}

fn rec_mut(vm: &mut Vm, m: usize) -> &mut Record {
    &mut vm.modules.records[m]
}

fn string(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

/// A SyntaxError (an early one, of a module's text or its links).
fn syntax(msg: impl std::fmt::Display) -> Error {
    Error::new_syntax(msg)
}

// ---- Loading ----

/// `specifier` from `referrer`'s file: a path, relative to its directory
/// (bare ones too: there are no packages), made canonical.
fn resolve_path(referrer: &str, specifier: &str) -> String {
    let base = std::path::Path::new(referrer).parent().unwrap_or(std::path::Path::new("."));
    let p = base.join(specifier);
    match std::fs::canonicalize(&p) {
        Ok(c) => c.to_string_lossy().into_owned(),
        Err(_) => p.to_string_lossy().into_owned(),
    }
}

/// The module at `path` (of kind `kind`), loaded with the modules it
/// requests (their graph), each once: its index. A file that can't be
/// read is an error; a syntax error in one, a SyntaxError.
fn load(vm: &mut Vm, path: &str, kind: Kind) -> Result<usize> {
    if let Some(&m) = vm.modules.by_path.get(&(path.to_string(), kind)) {
        return Ok(m);
    }
    let bytes = std::fs::read(path).map_err(|e| Error::type_error(format!("Cannot load module '{path}': {e}")))?;
    let m = vm.modules.records.len();
    let mut info = ModuleInfo::default();
    let env = object_value(vm.new_object(Value::NULL));
    let source = if kind == Kind::Bytes { String::new() } else { String::from_utf8_lossy(&bytes).into_owned() };
    if kind == Kind::Js {
        info = crate::js::module_info(&source, m as i32).map_err(|e| syntax(format!("{path}:{e}")))?;
    } else {
        let v = match kind {
            Kind::Json => {
                let u: Vec<u16> = source.encode_utf16().collect();
                crate::json::parse_json(vm, &u)?
            }
            Kind::Text => {
                let u: Vec<u16> = source.encode_utf16().collect();
                vm.new_string(&u)
            }
            _ => crate::typed::immutable_bytes(vm, &bytes)?,
        };
        let a = vm.intern("default");
        vm.set(env, a, v)?;
        info.local_exports.push(("default".into(), "default".into()));
    }
    vm.modules.records.push(Record {
        path: path.to_string(),
        kind,
        source,
        info,
        requested: Vec::new(),
        status: if kind == Kind::Js { Status::New } else { Status::Linked },
        env,
        namespace: Value::UNDEFINED,
        generator: Value::UNDEFINED,
        error: None,
        dfs_index: 0,
        dfs_ancestor: 0,
        cycle_root: m,
        async_order: None,
        was_async: false,
        pending_deps: 0,
        async_parents: Vec::new(),
        capability: None,
        meta: Value::UNDEFINED,
        exports: Vec::new(),
    });
    vm.modules.by_path.insert((path.to_string(), kind), m);
    let requests = rec(vm, m).info.requests.clone();
    let r = (|| {
        for (specifier, ty) in &requests {
            let k = request_type(ty.as_deref())?;
            let p = resolve_path(path, specifier);
            let dep = load(vm, &p, k)?;
            rec_mut(vm, m).requested.push(dep);
        }
        Ok(())
    })();
    if let Err(e) = r {
        // (Not kept: loading it again fails again, or not.)
        vm.modules.by_path.remove(&(path.to_string(), kind));
        return Err(e);
    }
    Ok(m)
}

/// The kind of module a request's `type` attribute asks for (none: a
/// JavaScript one); another is unsupported, a TypeError.
fn request_type(ty: Option<&str>) -> Result<Kind> {
    match ty {
        None => Ok(Kind::Js),
        Some("json") => Ok(Kind::Json),
        Some("text") => Ok(Kind::Text),
        Some("bytes") => Ok(Kind::Bytes),
        Some(t) => Err(Error::type_error(format!("Unsupported module type \"{t}\""))),
    }
}

// ---- Exports ----

/// What an export resolves to (ResolveExport's result).
#[derive(Clone, PartialEq)]
enum Resolution {
    /// A module's variable.
    Binding(usize, String),
    /// A module's namespace (`export * as ns from "m"`).
    Namespace(usize),
    Ambiguous,
}

/// ResolveExport (§16.2.1.6.3): what module `m`'s export `name` is, if
/// anything (`None`: nothing, or a cycle of re-exports).
fn resolve_export(vm: &Vm, m: usize, name: &str, set: &mut Vec<(usize, String)>) -> Option<Resolution> {
    if set.iter().any(|(k, n)| *k == m && n == name) {
        return None;
    }
    set.push((m, name.to_string()));
    let r = rec(vm, m);
    if let Some((_, local)) = r.info.local_exports.iter().find(|(e, _)| e == name) {
        return Some(Resolution::Binding(m, local.clone()));
    }
    if let Some((_, req, imported)) = r.info.indirect_exports.iter().find(|(e, ..)| e == name) {
        let dep = r.requested[*req];
        return match imported {
            None => Some(Resolution::Namespace(dep)),
            Some(n) => resolve_export(vm, dep, n, set),
        };
    }
    if name == "default" {
        return None;
    }
    let mut star: Option<Resolution> = None;
    for &req in &r.info.star_exports {
        let dep = r.requested[req];
        match resolve_export(vm, dep, name, set) {
            Some(Resolution::Ambiguous) => return Some(Resolution::Ambiguous),
            Some(res) => match &star {
                None => star = Some(res),
                Some(s) if *s != res => return Some(Resolution::Ambiguous),
                Some(_) => {}
            },
            None => {}
        }
    }
    star
}

/// GetExportedNames (§16.2.1.6.2).
fn exported_names(vm: &Vm, m: usize, set: &mut Vec<usize>) -> Vec<String> {
    if set.contains(&m) {
        return Vec::new();
    }
    set.push(m);
    let r = rec(vm, m);
    let mut names: Vec<String> = r.info.local_exports.iter().map(|(e, _)| e.clone()).collect();
    names.extend(r.info.indirect_exports.iter().map(|(e, ..)| e.clone()));
    for &req in &r.info.star_exports {
        let dep = r.requested[req];
        for n in exported_names(vm, dep, set) {
            if n != "default" && !names.contains(&n) {
                names.push(n);
            }
        }
    }
    names
}

/// GetModuleNamespace (§16.2.1.10): module `m`'s namespace object, made
/// once (see the module docs).
pub(crate) fn namespace(vm: &mut Vm, m: usize) -> Result<Value> {
    if !rec(vm, m).namespace.is_undefined() {
        return Ok(rec(vm, m).namespace);
    }
    let mut exports: Vec<(Vec<u16>, usize, Option<String>)> = Vec::new();
    for n in exported_names(vm, m, &mut Vec::new()) {
        match resolve_export(vm, m, &n, &mut Vec::new()) {
            Some(Resolution::Binding(k, local)) => exports.push((n.encode_utf16().collect(), k, Some(local))),
            Some(Resolution::Namespace(k)) => exports.push((n.encode_utf16().collect(), k, None)),
            _ => {}
        }
    }
    exports.sort_by(|a, b| a.0.cmp(&b.0));
    // The target: what the proxy's invariants check its traps against.
    let target = vm.new_object(Value::NULL);
    for (name, ..) in &exports {
        let s = vm.new_string(name);
        let key = crate::intrinsics::to_property_key(vm, s)?;
        vm.define_own_property(target, key, &Desc::data(Value::UNDEFINED, true, true, false))?;
    }
    let tag = string(vm, "Module");
    let a_tag = vm.a_sym_to_string_tag;
    vm.define_own_property(target, Key::Name(a_tag), &Desc::data(tag, false, false, false))?;
    vm.prevent_extensions(target);
    let handler = vm.new_object(Value::NULL);
    let traps = [Template::Get, Template::Set, Template::Has, Template::Delete, Template::Define, Template::GetOwn, Template::OwnKeys];
    let names = ["get", "set", "has", "deleteProperty", "defineProperty", "getOwnPropertyDescriptor", "ownKeys"];
    for (t, n) in traps.into_iter().zip(names) {
        let f = closure(vm, t, &[Value::from_i32(m as i32)]);
        let a = vm.intern(n);
        vm.set(object_value(handler), a, f)?;
    }
    let ns = crate::proxy::create(vm, object_value(target), object_value(handler))?;
    let r = rec_mut(vm, m);
    r.namespace = ns;
    r.exports = exports;
    Ok(ns)
}

/// The namespace's module (its traps' slot).
fn ns_module(vm: &Vm) -> usize {
    slot(vm.callee, 0).as_i32_unchecked() as usize
}

/// A trap's key: `None` for a symbol (the target's), else its string's
/// units.
fn ns_key(vm: &mut Vm, k: Value) -> Result<Option<Vec<u16>>> {
    if crate::symbol::is_symbol(k) {
        return Ok(None);
    }
    let s = if k.is_number() { crate::intrinsics::to_string(vm, k)? } else { k };
    Ok(Some(string_units(s).unwrap_or(&[]).to_vec()))
}

/// The binding of export `key` of the namespace's module, if it has one.
fn ns_binding(vm: &Vm, m: usize, key: &[u16]) -> Option<(usize, Option<String>)> {
    let e = &rec(vm, m).exports;
    e.binary_search_by(|(n, ..)| n.as_slice().cmp(key)).ok().map(|i| (e[i].1, e[i].2.clone()))
}

/// A binding's value: module `k`'s variable, live (a ReferenceError while
/// uninitialized), or its namespace.
fn binding_value(vm: &mut Vm, k: usize, local: Option<&str>, name: &[u16]) -> Result<Value> {
    let Some(local) = local else { return namespace(vm, k) };
    let env = rec(vm, k).env;
    let a = vm.intern(local);
    let v = vm.get(env, a)?;
    if v == Value::HOLE {
        let n = String::from_utf16_lossy(name);
        return Err(Error::reference_error(format!("Cannot access '{n}' before initialization")));
    }
    Ok(v)
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// [[Get]]: the binding's value; a symbol's, the target's.
fn ns_get(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let m = ns_module(vm);
    let Some(key) = ns_key(vm, arg(a, 1))? else {
        return crate::reflect::reflect_get(vm, this, a);
    };
    match ns_binding(vm, m, &key) {
        Some((k, local)) => binding_value(vm, k, local.as_deref(), &key),
        None => Ok(Value::UNDEFINED),
    }
}

/// [[Set]]: never.
fn ns_set(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::FALSE)
}

/// [[HasProperty]]: an export (a symbol: the target's).
fn ns_has(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let m = ns_module(vm);
    match ns_key(vm, arg(a, 1))? {
        None => crate::reflect::reflect_has(vm, this, a),
        Some(key) => Ok(Value::from_bool(ns_binding(vm, m, &key).is_some())),
    }
}

/// [[Delete]]: anything but an export.
fn ns_delete(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let m = ns_module(vm);
    match ns_key(vm, arg(a, 1))? {
        None => crate::reflect::reflect_delete_property(vm, this, a),
        Some(key) => Ok(Value::from_bool(ns_binding(vm, m, &key).is_none())),
    }
}

/// [[DefineOwnProperty]]: true only if the descriptor is the export's
/// (writable, enumerable, not configurable, its value).
fn ns_define(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let m = ns_module(vm);
    let Some(key) = ns_key(vm, arg(a, 1))? else {
        return crate::reflect::reflect_define_property(vm, this, a);
    };
    let Some((k, local)) = ns_binding(vm, m, &key) else {
        return Ok(Value::FALSE);
    };
    let current = binding_value(vm, k, local.as_deref(), &key)?;
    let d = vm.to_descriptor(arg(a, 2))?;
    let ok = d.configurable != Some(true)
        && d.enumerable != Some(false)
        && !d.is_accessor()
        && d.writable != Some(false)
        && d.value.is_none_or(|v| same_value(v, current));
    Ok(Value::from_bool(ok))
}

/// [[GetOwnProperty]]: an export's value, writable, enumerable, not
/// configurable.
fn ns_get_own(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let m = ns_module(vm);
    let Some(key) = ns_key(vm, arg(a, 1))? else {
        return crate::reflect::reflect_get_own_property_descriptor(vm, this, a);
    };
    match ns_binding(vm, m, &key) {
        Some((k, local)) => {
            let v = binding_value(vm, k, local.as_deref(), &key)?;
            Ok(vm.from_descriptor(&Desc::data(v, true, true, false)))
        }
        None => Ok(Value::UNDEFINED),
    }
}

/// [[OwnPropertyKeys]]: the exports' names, in order, then
/// `Symbol.toStringTag`.
fn ns_own_keys(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let m = ns_module(vm);
    let names: Vec<Vec<u16>> = rec(vm, m).exports.iter().map(|(n, ..)| n.clone()).collect();
    let out = vm.new_array();
    vm.handle_scope(|vm| {
        vm.handles.push(out);
        for n in names {
            let s = vm.new_string(&n);
            crate::vm::array_push(vm, out, &[s])?;
        }
        let tag = vm.key_value(Key::Name(vm.a_sym_to_string_tag));
        crate::vm::array_push(vm, out, &[tag])?;
        Ok(out)
    })
}

// ---- Linking ----

/// Link module `root` and the modules it requests (§16.2.1.5.1): each
/// one's imports resolved (a SyntaxError if one can't be), its code
/// compiled and its function called (its bindings made). If one fails,
/// those linked here are unlinked again.
fn link(vm: &mut Vm, root: usize) -> Result<()> {
    let mut done = Vec::new();
    let r = link_from(vm, root, &mut done);
    if r.is_err() {
        for m in done {
            let r = rec_mut(vm, m);
            r.status = Status::New;
            r.generator = Value::UNDEFINED;
        }
    }
    r
}

fn link_from(vm: &mut Vm, m: usize, done: &mut Vec<usize>) -> Result<()> {
    if rec(vm, m).status != Status::New {
        return Ok(());
    }
    rec_mut(vm, m).status = Status::Linking;
    done.push(m);
    for dep in rec(vm, m).requested.clone() {
        link_from(vm, dep, done)?;
    }
    initialize(vm, m)?;
    rec_mut(vm, m).status = Status::Linked;
    Ok(())
}

/// InitializeEnvironment (§16.2.1.6.4): module `m`'s re-exports checked,
/// its imports resolved, its function made and called with its scope
/// object and its imports' holders.
fn initialize(vm: &mut Vm, m: usize) -> Result<()> {
    let r = rec(vm, m);
    let path = r.path.clone();
    let indirect: Vec<String> = r.info.indirect_exports.iter().map(|(e, ..)| e.clone()).collect();
    let imports = r.info.imports.clone();
    let requested = r.requested.clone();
    for e in indirect {
        match resolve_export(vm, m, &e, &mut Vec::new()) {
            Some(Resolution::Binding(..) | Resolution::Namespace(_)) => {}
            Some(Resolution::Ambiguous) => return Err(syntax(format!("{path}: the export '{e}' is ambiguous"))),
            None => return Err(syntax(format!("{path}: the export '{e}' can't be resolved"))),
        }
    }
    let mut holders = vec![rec(vm, m).env];
    let mut links = Vec::new();
    for (req, name, _) in &imports {
        let dep = requested[*req];
        let res = match name {
            None => Resolution::Namespace(dep),
            Some(n) => match resolve_export(vm, dep, n, &mut Vec::new()) {
                Some(Resolution::Ambiguous) => {
                    return Err(syntax(format!("{path}: the requested module '{}' has an ambiguous export '{n}'", rec(vm, dep).path)));
                }
                None => {
                    return Err(syntax(format!("{path}: the requested module '{}' provides no export named '{n}'", rec(vm, dep).path)));
                }
                Some(res) => res,
            },
        };
        match res {
            Resolution::Binding(k, local) => {
                holders.push(rec(vm, k).env);
                links.push(Some(local));
            }
            Resolution::Namespace(k) => {
                let ns = namespace(vm, k)?;
                holders.push(ns);
                links.push(None);
            }
            Resolution::Ambiguous => unreachable!(),
        }
    }
    let module = crate::js::compile_module(&rec(vm, m).source, m as i32, &links).map_err(|e| {
        if e.contains("unsupported") { Error::unsupported(format!("{path}:{e}")) } else { syntax(format!("{path}:{e}")) }
    })?;
    let code = vm.load(module);
    vm.handle_scope(|vm| {
        vm.handles.extend_from_slice(&holders);
        let f = crate::interp::run_script_at(vm, code)?;
        vm.handles.push(f);
        let g = call_function(vm, f, Value::UNDEFINED, &holders)?;
        rec_mut(vm, m).generator = g;
        Ok(())
    })
}

// ---- Evaluation ----

/// Evaluate (§16.2.1.5.3): a promise of module `m`'s evaluation (and its
/// graph's), settled now if none of it is async.
fn evaluate(vm: &mut Vm, m: usize) -> Result<Value> {
    let mut m = m;
    if matches!(rec(vm, m).status, Status::EvaluatingAsync | Status::Evaluated) {
        m = rec(vm, m).cycle_root;
    }
    if let Some((p, ..)) = rec(vm, m).capability {
        return Ok(p);
    }
    let cap = crate::promise::new_with_resolvers(vm);
    rec_mut(vm, m).capability = Some(cap);
    let mut stack = Vec::new();
    match inner_evaluate(vm, m, &mut stack, 0) {
        Err(e) => {
            let v = thrown(vm, e)?;
            for k in stack {
                let r = rec_mut(vm, k);
                r.status = Status::Evaluated;
                r.error = Some(v);
            }
            call_function(vm, cap.2, Value::UNDEFINED, &[v])?;
        }
        Ok(_) => {
            if rec(vm, m).async_order.is_none() {
                call_function(vm, cap.1, Value::UNDEFINED, &[Value::UNDEFINED])?;
            }
        }
    }
    Ok(cap.0)
}

/// The value thrown for `e` (an error object for the runtime's own); an
/// `unsupported` error stays one.
fn thrown(vm: &mut Vm, e: Error) -> Result<Value> {
    match e.1 {
        Some(v) => Ok(v),
        None if e.0.starts_with("unsupported") => Err(e),
        None => Ok(crate::builtins::caught(vm, &e)),
    }
}

/// InnerModuleEvaluation (§16.2.1.5.3.1).
fn inner_evaluate(vm: &mut Vm, m: usize, stack: &mut Vec<usize>, index: usize) -> Result<usize> {
    let mut index = index;
    match rec(vm, m).status {
        Status::EvaluatingAsync | Status::Evaluated => {
            return match rec(vm, m).error {
                Some(v) => Err(Error::thrown(v)),
                None => Ok(index),
            };
        }
        Status::Evaluating => return Ok(index),
        _ => {}
    }
    {
        let r = rec_mut(vm, m);
        r.status = Status::Evaluating;
        r.dfs_index = index;
        r.dfs_ancestor = index;
        r.pending_deps = 0;
    }
    index += 1;
    stack.push(m);
    for dep in rec(vm, m).requested.clone() {
        index = inner_evaluate(vm, dep, stack, index)?;
        let mut dep = dep;
        if rec(vm, dep).status == Status::Evaluating {
            let a = rec(vm, dep).dfs_ancestor.min(rec(vm, m).dfs_ancestor);
            rec_mut(vm, m).dfs_ancestor = a;
        } else {
            dep = rec(vm, dep).cycle_root;
            if let Some(v) = rec(vm, dep).error {
                return Err(Error::thrown(v));
            }
        }
        if rec(vm, dep).async_order.is_some() {
            rec_mut(vm, m).pending_deps += 1;
            rec_mut(vm, dep).async_parents.push(m);
        }
    }
    if rec(vm, m).pending_deps > 0 || rec(vm, m).info.has_await {
        let order = vm.modules.async_order;
        vm.modules.async_order += 1;
        let r = rec_mut(vm, m);
        r.async_order = Some(order);
        r.was_async = true;
        if r.pending_deps == 0 {
            execute_async(vm, m)?;
        }
    } else {
        execute(vm, m)?;
    }
    if rec(vm, m).dfs_ancestor == rec(vm, m).dfs_index {
        loop {
            let k = stack.pop().unwrap();
            let r = rec_mut(vm, k);
            r.status = if r.async_order.is_none() { Status::Evaluated } else { Status::EvaluatingAsync };
            r.cycle_root = m;
            if k == m {
                break;
            }
        }
    }
    Ok(index)
}

/// ExecuteModule, synchronously: its body run.
fn execute(vm: &mut Vm, m: usize) -> Result<()> {
    if rec(vm, m).kind != Kind::Js {
        return Ok(());
    }
    let g = rec(vm, m).generator;
    crate::generator::run_module(vm, g)
}

/// ExecuteAsyncModule (§16.2.1.5.3.2): its body run as an async function
/// is, its end or exception continuing the evaluation of the modules
/// waiting for it.
fn execute_async(vm: &mut Vm, m: usize) -> Result<()> {
    let g = rec(vm, m).generator;
    let p = crate::generator::async_start(vm, g)?;
    let on_fulfilled = closure(vm, Template::Fulfilled, &[Value::from_i32(m as i32)]);
    let on_rejected = closure(vm, Template::Rejected, &[Value::from_i32(m as i32)]);
    crate::promise::await_then(vm, p, on_fulfilled, on_rejected)
}

/// GatherAvailableAncestors (§16.2.1.5.3.3).
fn gather_ancestors(vm: &mut Vm, m: usize, out: &mut Vec<usize>) {
    for parent in rec(vm, m).async_parents.clone() {
        let root = rec(vm, parent).cycle_root;
        if !out.contains(&parent) && rec(vm, root).error.is_none() {
            let r = rec_mut(vm, parent);
            r.pending_deps -= 1;
            if r.pending_deps == 0 {
                out.push(parent);
                if !r.info.has_await {
                    gather_ancestors(vm, parent, out);
                }
            }
        }
    }
}

/// AsyncModuleExecutionFulfilled (§16.2.1.5.3.4), module in slot 0.
fn async_fulfilled(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let m = slot(vm.callee, 0).as_i32_unchecked() as usize;
    module_fulfilled(vm, m)?;
    Ok(Value::UNDEFINED)
}

fn module_fulfilled(vm: &mut Vm, m: usize) -> Result<()> {
    if rec(vm, m).status == Status::Evaluated {
        return Ok(());
    }
    {
        let r = rec_mut(vm, m);
        r.async_order = None;
        r.status = Status::Evaluated;
    }
    if let Some((_, resolve, _)) = rec(vm, m).capability {
        call_function(vm, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?;
    }
    let mut ready = Vec::new();
    gather_ancestors(vm, m, &mut ready);
    // (In the order they were found async.)
    ready.sort_by_key(|&k| rec(vm, k).async_order);
    for k in ready {
        if rec(vm, k).status == Status::Evaluated {
            continue;
        }
        if rec(vm, k).info.has_await {
            execute_async(vm, k)?;
            continue;
        }
        match execute(vm, k) {
            Err(e) => {
                let v = thrown(vm, e)?;
                module_rejected(vm, k, v)?;
            }
            Ok(()) => {
                let r = rec_mut(vm, k);
                r.async_order = None;
                r.status = Status::Evaluated;
                if let Some((_, resolve, _)) = r.capability {
                    call_function(vm, resolve, Value::UNDEFINED, &[Value::UNDEFINED])?;
                }
            }
        }
    }
    Ok(())
}

/// AsyncModuleExecutionRejected (§16.2.1.5.3.5), module in slot 0.
fn async_rejected(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let m = slot(vm.callee, 0).as_i32_unchecked() as usize;
    module_rejected(vm, m, arg(a, 0))?;
    Ok(Value::UNDEFINED)
}

fn module_rejected(vm: &mut Vm, m: usize, error: Value) -> Result<()> {
    if rec(vm, m).status == Status::Evaluated {
        return Ok(());
    }
    {
        let r = rec_mut(vm, m);
        r.error = Some(error);
        r.status = Status::Evaluated;
        r.async_order = None;
    }
    // (Its own promise first, then those of the modules waiting.)
    if let Some((_, _, reject)) = rec(vm, m).capability {
        call_function(vm, reject, Value::UNDEFINED, &[error])?;
    }
    for parent in rec(vm, m).async_parents.clone() {
        module_rejected(vm, parent, error)?;
    }
    Ok(())
}

// ---- The host ----

/// Run module `path` (the program's): loaded, linked, evaluated, its
/// jobs run (by the caller). `Err(Ok(message))`: an early error (a
/// syntax error, an import that can't be resolved); `Err(Err(e))`: what
/// it threw. Else: the promise of its evaluation.
pub(crate) fn run_main(vm: &mut Vm, path: &str) -> std::result::Result<Value, std::result::Result<String, Error>> {
    let path = resolve_path(".", path);
    let m = load(vm, &path, Kind::Js).map_err(early)?;
    link(vm, m).map_err(early)?;
    evaluate(vm, m).map_err(Err)
}

/// An error of loading or linking: its message, unless it isn't early
/// (`unsupported`, a file that can't be read).
fn early(e: Error) -> std::result::Result<String, Error> {
    if e.0.starts_with("SyntaxError") { Ok(e.0) } else { Err(e) }
}

/// `import(specifier, options)` from module `referrer` (-1: the script):
/// a promise of the module's namespace. The specifier and the options are
/// read now (what that throws rejects it), the module loaded, linked and
/// evaluated in a job (what fails there rejects it too).
pub(crate) fn dynamic_import(vm: &mut Vm, specifier: Value, options: Value, referrer: Value) -> Result<Value> {
    let (p, resolve, reject) = crate::promise::new_with_resolvers(vm);
    match import_request(vm, specifier, options) {
        Ok((spec, ty)) => {
            let job = closure(vm, Template::ImportJob, &[spec, ty, referrer, resolve, reject]);
            crate::promise::await_then(vm, Value::UNDEFINED, job, Value::UNDEFINED)?;
        }
        Err(e) => {
            let v = thrown(vm, e)?;
            call_function(vm, reject, Value::UNDEFINED, &[v])?;
        }
    }
    Ok(p)
}

/// `import()`'s specifier, a string, and its `type` attribute (or
/// undefined), from its options (`{ with: { type: "json" } }`: each
/// attribute a string).
fn import_request(vm: &mut Vm, specifier: Value, options: Value) -> Result<(Value, Value)> {
    let spec = crate::intrinsics::to_string(vm, specifier)?;
    let mut ty = Value::UNDEFINED;
    if !options.is_undefined() {
        if !crate::props::is_object(options) {
            return Err(Error::type_error("The second argument of import() must be an object"));
        }
        let a_with = vm.intern("with");
        let attrs = vm.get(options, a_with)?;
        if !attrs.is_undefined() {
            if !crate::props::is_object(attrs) {
                return Err(Error::type_error("The 'with' option of import() must be an object"));
            }
            // (EnumerableOwnProperties: through a proxy's traps too.)
            let o = as_object(attrs);
            for key in crate::proxy::own_keys(vm, o)? {
                if crate::symbol::is_symbol(vm.key_value(key)) {
                    continue;
                }
                if !crate::proxy::get_own_property(vm, o, key)?.is_some_and(|d| d.enumerable == Some(true)) {
                    continue;
                }
                let v = vm.get_key(attrs, key)?;
                if string_units(v).is_none() {
                    return Err(Error::type_error("Import attribute values must be strings"));
                }
                let k = vm.key_string(key);
                if string_units(k) == Some(&"type".encode_utf16().collect::<Vec<u16>>()[..]) {
                    ty = v;
                }
            }
        }
    }
    Ok((spec, ty))
}

/// `import()`'s job (slots: the specifier, its type, the referrer,
/// resolve, reject): the module loaded and linked, its evaluation's
/// promise then resolving `import()`'s with its namespace.
fn import_job(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let me = vm.callee;
    let (spec, ty, referrer, resolve, reject) = (slot(me, 0), slot(me, 1), slot(me, 2), slot(me, 3), slot(me, 4));
    let r = (|| {
        let spec = String::from_utf16_lossy(string_units(spec).unwrap_or(&[]));
        let ty = string_units(ty).map(String::from_utf16_lossy);
        let kind = request_type(ty.as_deref())?;
        let k = referrer.as_i32_unchecked();
        let from = if k >= 0 {
            rec(vm, k as usize).path.clone()
        } else {
            vm.modules.script_path.clone().unwrap_or_else(|| "./script".into())
        };
        let path = resolve_path(&from, &spec);
        let m = load(vm, &path, kind)?;
        link(vm, m)?;
        let p = evaluate(vm, m)?;
        let done = closure(vm, Template::ImportDone, &[Value::from_i32(m as i32), resolve]);
        crate::promise::await_then(vm, p, done, reject)
    })();
    if let Err(e) = r {
        let v = thrown(vm, e)?;
        call_function(vm, reject, Value::UNDEFINED, &[v])?;
    }
    Ok(Value::UNDEFINED)
}

/// `import()`'s module evaluated (slots: the module, resolve): its
/// namespace resolves the promise.
fn import_done(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let me = vm.callee;
    let (m, resolve) = (slot(me, 0).as_i32_unchecked() as usize, slot(me, 1));
    match namespace(vm, m) {
        Ok(ns) => call_function(vm, resolve, Value::UNDEFINED, &[ns]),
        Err(e) => Err(e),
    }
}

/// `import.meta` of module `m`: an object of its own, with a null
/// prototype, made once.
pub(crate) fn import_meta(vm: &mut Vm, m: Value) -> Result<Value> {
    let m = m.as_i32_unchecked() as usize;
    if rec(vm, m).meta.is_undefined() {
        let o = object_value(vm.new_object(Value::NULL));
        rec_mut(vm, m).meta = o;
    }
    Ok(rec(vm, m).meta)
}
