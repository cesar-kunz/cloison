//! Symbols, and the iteration protocol built on `Symbol.iterator`.
//!
//! A symbol is a cell of class `Symbol` (a primitive to JS: `typeof` is
//! "symbol", `is_object` false), whose prototype is `Symbol.prototype`.
//! As a property key it is an atom of its own, named `\u{FDD0}` and a
//! number (or the well-known symbol's name), which string keys don't
//! reach in practice; `own_keys` leaves those out (see
//! [`is_symbol_atom`]). The cell keeps its atom in `elems_len` and its
//! description in `code`'s bits (traced: see `gc`).

use crate::interp::{call_function, to_boolean};
use crate::object::{Atom, Class, DONT_ENUM, Object, as_object, object_value};
use crate::props::{Desc, is_callable, is_object};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, string_units};

/// The first character of a symbol atom's name.
pub(crate) const SYMBOL_MARK: char = '\u{FDD0}';

/// The well-known symbols, as `Symbol`'s properties.
const WELL_KNOWN: &[&str] = &[
    "iterator",
    "asyncIterator",
    "hasInstance",
    "isConcatSpreadable",
    "match",
    "matchAll",
    "replace",
    "search",
    "species",
    "split",
    "toPrimitive",
    "toStringTag",
    "unscopables",
];

/// Indices into `Vm::symbol_roots`.
#[derive(Clone, Copy)]
#[repr(usize)]
#[allow(dead_code)]
pub(crate) enum Root {
    SymbolPrototype,
    IteratorPrototype,
    ArrayIteratorPrototype,
    StringIteratorPrototype,
    ArrayValues,
    GeneratorPrototype,
    AsyncGeneratorPrototype,
}

fn root(vm: &Vm, r: Root) -> *mut Object {
    vm.symbol_roots[r as usize]
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// Whether `v` is a symbol.
pub(crate) fn is_symbol(v: Value) -> bool {
    v.is_cell() && unsafe { (*as_object(v)).class } == Class::Symbol
}

/// Symbol `v`'s property key.
pub(crate) fn symbol_atom(v: Value) -> Atom {
    Atom(unsafe { (*as_object(v)).elems_len })
}

/// Whether atom `a` is a symbol's.
pub(crate) fn is_symbol_atom(vm: &Vm, a: Atom) -> bool {
    vm.atom_name(a).starts_with(SYMBOL_MARK)
}

/// Whether atom `a` is a private name's (see [`new_private_symbol`]).
pub(crate) fn is_private_atom(vm: &Vm, a: Atom) -> bool {
    let n = vm.atom_name(a);
    n.starts_with(SYMBOL_MARK) && n[SYMBOL_MARK.len_utf8()..].starts_with('#')
}

/// A new private name's symbol, described `desc` (`#x`): its key no
/// reflection lists.
pub(crate) fn new_private_symbol(vm: &mut Vm, desc: Value) -> Value {
    vm.symbol_count += 1;
    let name = format!("{SYMBOL_MARK}#{}", vm.symbol_count);
    make_symbol(vm, desc, Some(name))
}

/// Symbol `v`'s description as a string (a private name's `#x`).
pub(crate) fn description_string(v: Value) -> String {
    String::from_utf16_lossy(crate::vm::string_units(description(v)).unwrap_or(&[]))
}

/// The symbol whose key atom `a` is.
pub(crate) fn atom_symbol(vm: &Vm, a: Atom) -> Option<Value> {
    vm.symbols.get(&a).map(|&o| object_value(o))
}

/// A symbol's description (`undefined` or a string).
fn description(v: Value) -> Value {
    Value::from_bits(unsafe { (*as_object(v)).code } as u64)
}

/// A new symbol, its key atom named `name` (a new one if `None`).
fn make_symbol(vm: &mut Vm, desc: Value, name: Option<String>) -> Value {
    let name = name.unwrap_or_else(|| {
        vm.symbol_count += 1;
        format!("{SYMBOL_MARK}{}", vm.symbol_count)
    });
    let atom = vm.intern(&name);
    let proto = object_value(root(vm, Root::SymbolPrototype));
    let o = vm.new_object_with(Class::Symbol, proto, 0);
    unsafe {
        (*o).code = desc.to_bits() as *const crate::vm::Code;
        (*o).elems_len = atom.0;
    }
    vm.prevent_extensions(o);
    vm.symbols.insert(atom, o);
    object_value(o)
}

/// `Symbol.prototype.toString` of symbol `v`: `Symbol(description)`.
pub(crate) fn symbol_string(v: Value) -> Vec<u16> {
    let mut u: Vec<u16> = "Symbol(".encode_utf16().collect();
    if let Some(d) = string_units(description(v)) {
        u.extend_from_slice(d);
    }
    u.push(b')' as u16);
    u
}

/// The symbol `this` is (a symbol, or a `Symbol` wrapper).
fn this_symbol(this: Value, what: &str) -> Result<Value> {
    if is_symbol(this) {
        return Ok(this);
    }
    if let Some(v) = crate::vm::wrapped(this)
        && is_symbol(v)
    {
        return Ok(v);
    }
    Err(Error::type_error(format!("{what} requires that 'this' be a Symbol")))
}

fn symbol_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if vm.is_construct() {
        return Err(Error::type_error("Symbol is not a constructor"));
    }
    let d = arg(a, 0);
    let desc = if d.is_undefined() { d } else { crate::intrinsics::to_string(vm, d)? };
    Ok(make_symbol(vm, desc, None))
}

fn symbol_for(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let key = crate::intrinsics::to_string(vm, arg(a, 0))?;
    let units = string_units(key).unwrap_or(&[]).to_vec();
    if let Some(&s) = vm.symbol_registry.get(&units) {
        return Ok(object_value(s));
    }
    let s = make_symbol(vm, key, None);
    vm.symbol_registry.insert(units, as_object(s));
    Ok(s)
}

fn symbol_key_for(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let s = arg(a, 0);
    if !is_symbol(s) {
        return Err(Error::type_error(format!("{} is not a symbol", crate::vm::display(s))));
    }
    let o = as_object(s);
    Ok(if vm.symbol_registry.values().any(|&r| r == o) { description(s) } else { Value::UNDEFINED })
}

fn symbol_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let s = this_symbol(this, "Symbol.prototype.toString")?;
    let u = symbol_string(s);
    Ok(vm.new_string(&u))
}

fn symbol_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    this_symbol(this, "Symbol.prototype.valueOf")
}

fn symbol_description(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(description(this_symbol(this, "Symbol.prototype.description")?))
}

/// `Object.getOwnPropertySymbols(o)`.
fn get_own_property_symbols(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let o = vm.to_object(arg(a, 0))?;
    let keys: Vec<Value> = if vm.proxies_exist && crate::proxy::is_proxy(vm, o) {
        let ks = crate::proxy::own_keys(vm, o)?;
        ks.into_iter()
            .filter_map(|k| match k {
                Key::Name(a) => atom_symbol(vm, a),
                Key::Index(_) => None,
            })
            .collect()
    } else {
        own_symbol_keys(vm, o)
    };
    let out = vm.new_array();
    for s in keys {
        crate::vm::array_push(vm, out, &[s])?;
    }
    Ok(out)
}

/// `o`'s own symbol-keyed properties' symbols, in order.
pub(crate) fn own_symbol_keys(vm: &mut Vm, o: *mut Object) -> Vec<Value> {
    let keys: Vec<Atom> = unsafe { (*(*o).shape).keys.to_vec() };
    keys.into_iter()
        .filter(|&a| is_symbol_atom(vm, a) && !is_private_atom(vm, a))
        .filter_map(|a| atom_symbol(vm, a))
        .collect()
}

/// Define native `f` on `o` under symbol key `a`, named `name`.
fn define_symbol_native(vm: &mut Vm, o: *mut Object, a: Atom, name: &str, f: NativeFn, n: u32) -> *mut Object {
    let fo = vm.native_function(name, f, n, crate::jit::Ty::Any, false);
    vm.add_property_with(o, a, object_value(fo), DONT_ENUM);
    fo
}

/// A getter `name` on `o`.
fn define_getter(vm: &mut Vm, o: *mut Object, key: Key, name: &str, f: NativeFn) {
    let g = vm.native_function(&format!("get {name}"), f, 0, crate::jit::Ty::Any, false);
    let d = Desc {
        get: Some(object_value(g)),
        set: Some(Value::UNDEFINED),
        enumerable: Some(false),
        configurable: Some(true),
        ..Default::default()
    };
    let _ = vm.define_own_property(o, key, &d);
}

/// `o[@@toStringTag] = tag`, read-only.
pub(crate) fn to_string_tag(vm: &mut Vm, o: *mut Object, tag: &str) {
    let u: Vec<u16> = tag.encode_utf16().collect();
    let s = vm.string_constant(&u);
    let a = vm.a_sym_to_string_tag;
    let d = Desc::data(s, false, false, true);
    let _ = vm.define_own_property(o, Key::Name(a), &d);
}

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let sp = vm.new_object(op);
    vm.symbol_roots = vec![sp];
    let ctor = vm.define_native(g, "Symbol", symbol_ctor, 0, crate::jit::Ty::Any, true);
    let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
    let d = Desc::data(object_value(sp), false, false, false);
    let _ = vm.define_own_property(ctor, Key::Name(a_p), &d);
    vm.add_property_with(sp, a_c, object_value(ctor), DONT_ENUM);
    for &w in WELL_KNOWN {
        let desc_units: Vec<u16> = format!("Symbol.{w}").encode_utf16().collect();
        let desc = vm.string_constant(&desc_units);
        let s = make_symbol(vm, desc, Some(format!("{SYMBOL_MARK}Symbol.{w}")));
        let a = vm.intern(w);
        let d = Desc::data(s, false, false, false);
        let _ = vm.define_own_property(ctor, Key::Name(a), &d);
    }
    vm.a_sym_iterator = vm.intern(&format!("{SYMBOL_MARK}Symbol.iterator"));
    vm.a_sym_species = vm.intern(&format!("{SYMBOL_MARK}Symbol.species"));
    vm.a_sym_to_string_tag = vm.intern(&format!("{SYMBOL_MARK}Symbol.toStringTag"));
    vm.a_sym_to_primitive = vm.intern(&format!("{SYMBOL_MARK}Symbol.toPrimitive"));
    vm.a_sym_has_instance = vm.intern(&format!("{SYMBOL_MARK}Symbol.hasInstance"));
    vm.define_natives(ctor, &[("for", symbol_for as NativeFn, 1), ("keyFor", symbol_key_for, 1)]);
    vm.define_natives(sp, &[("toString", symbol_to_string as NativeFn, 0), ("valueOf", symbol_value_of, 0)]);
    let a_description = vm.intern("description");
    define_getter(vm, sp, Key::Name(a_description), "description", symbol_description);
    let tp = vm.a_sym_to_primitive;
    let f = vm.native_function("[Symbol.toPrimitive]", symbol_value_of, 1, crate::jit::Ty::Any, false);
    let d = Desc::data(object_value(f), false, false, true);
    let _ = vm.define_own_property(sp, Key::Name(tp), &d);
    to_string_tag(vm, sp, "Symbol");
    let object_ctor = { let a = vm.intern("Object"); vm.get(object_value(g), a) }.unwrap_or(Value::UNDEFINED);
    if object_ctor.is_cell() {
        vm.define_natives(as_object(object_ctor), &[("getOwnPropertySymbols", get_own_property_symbols as NativeFn, 1)]);
    }

    // `@@species` getters: the constructor itself.
    let a_species = vm.intern(&format!("{SYMBOL_MARK}Symbol.species"));
    for name in ["Array", "Map", "Set", "Promise", "RegExp", "ArrayBuffer"] {
        let a = vm.intern(name);
        let c = vm.get(object_value(g), a).unwrap_or(Value::UNDEFINED);
        if is_object(c) {
            define_getter(vm, as_object(c), Key::Name(a_species), "[Symbol.species]", return_this);
        }
    }

    // Iterators.
    let ip = vm.new_object(op);
    vm.symbol_roots.push(ip);
    let a_iter = vm.a_sym_iterator;
    define_symbol_native(vm, ip, a_iter, "[Symbol.iterator]", return_this, 0);
    let aip = vm.new_object(object_value(ip));
    vm.symbol_roots.push(aip);
    vm.define_natives(aip, &[("next", array_iterator_next as NativeFn, 0)]);
    to_string_tag(vm, aip, "Array Iterator");
    let sip = vm.new_object(object_value(ip));
    vm.symbol_roots.push(sip);
    vm.define_natives(sip, &[("next", string_iterator_next as NativeFn, 0)]);
    to_string_tag(vm, sip, "String Iterator");

    let ap = vm.array_prototype;
    vm.define_natives(ap, &[("keys", array_keys as NativeFn, 0), ("entries", array_entries, 0)]);
    let values = vm.define_native(ap, "values", array_values, 0, crate::jit::Ty::Any, false);
    vm.symbol_roots.push(values);
    vm.add_property_with(ap, a_iter, object_value(values), DONT_ENUM);
    // (`%TypedArray%`'s `@@species`: the kinds' constructors' prototype.)
    if let Some(&c) = vm.typed_constructors.first() {
        let ta = as_object(unsafe { (*(*c).shape).proto });
        define_getter(vm, ta, Key::Name(a_species), "[Symbol.species]", return_this);
        crate::typed::install_symbols(vm);
    }
    let strp = vm.string_prototype;
    define_symbol_native(vm, strp, a_iter, "[Symbol.iterator]", string_iterator, 0);

    // Maps' and sets' iterators: `@@iterator`, and `%IteratorPrototype%`.
    use crate::collections::Proto;
    let cp = vm.collection_prototypes.clone();
    for (p, method, tag) in [(Proto::Map, "entries", "Map"), (Proto::Set, "values", "Set")] {
        let o = cp[p as usize];
        let a = vm.intern(method);
        let f = vm.get(object_value(o), a).unwrap_or(Value::UNDEFINED);
        vm.add_property_with(o, a_iter, f, DONT_ENUM);
        to_string_tag(vm, o, tag);
    }
    for (p, tag) in [(Proto::MapIterator, "Map Iterator"), (Proto::SetIterator, "Set Iterator")] {
        let o = cp[p as usize];
        vm.set_prototype_of(o, object_value(ip));
        to_string_tag(vm, o, tag);
    }
    to_string_tag(vm, cp[Proto::WeakMap as usize], "WeakMap");
    to_string_tag(vm, cp[Proto::WeakSet as usize], "WeakSet");
    to_string_tag(vm, cp[Proto::WeakRef as usize], "WeakRef");
    to_string_tag(vm, cp[Proto::FinalizationRegistry as usize], "FinalizationRegistry");
    to_string_tag(vm, cp[Proto::DataView as usize], "DataView");
    let math = { let a = vm.intern("Math"); vm.get(object_value(g), a) }.unwrap_or(Value::UNDEFINED);
    if is_object(math) {
        to_string_tag(vm, as_object(math), "Math");
    }
    for name in ["JSON", "Reflect"] {
        let o = { let a = vm.intern(name); vm.get(object_value(g), a) }.unwrap_or(Value::UNDEFINED);
        if is_object(o) {
            to_string_tag(vm, as_object(o), name);
        }
    }
}

fn return_this(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(this)
}

// ---- Array and string iterators ----

/// An iterator over `target` of kind `kind` (0 keys, 1 values, 2
/// entries; 3 a string's code points): a `CollectionIterator`-like
/// object of class `ArrayIterator`, its state a hidden array `[target,
/// index, kind]` in `code`.
fn new_iterator(vm: &mut Vm, target: Value, kind: i32, proto: Root) -> Result<Value> {
    let state = crate::array::new_array_of(vm, &[target, Value::from_i32(0), Value::from_i32(kind)])?;
    let p = object_value(root(vm, proto));
    let it = vm.new_object_with(Class::ArrayIterator, p, 0);
    unsafe { (*it).code = state.to_bits() as *const crate::vm::Code };
    Ok(object_value(it))
}

fn this_target(vm: &mut Vm, this: Value) -> Result<Value> {
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error("Array.prototype iterator called on null or undefined"));
    }
    Ok(object_value(vm.to_object(this)?))
}

pub(crate) fn array_values(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let t = this_target(vm, this)?;
    new_iterator(vm, t, 1, Root::ArrayIteratorPrototype)
}

/// An array iterator over `target` of kind `kind` (see `new_iterator`).
pub(crate) fn array_iterator(vm: &mut Vm, target: Value, kind: i32) -> Result<Value> {
    new_iterator(vm, target, kind, Root::ArrayIteratorPrototype)
}

fn array_keys(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let t = this_target(vm, this)?;
    new_iterator(vm, t, 0, Root::ArrayIteratorPrototype)
}

fn array_entries(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let t = this_target(vm, this)?;
    new_iterator(vm, t, 2, Root::ArrayIteratorPrototype)
}

fn string_iterator(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error("String.prototype[Symbol.iterator] called on null or undefined"));
    }
    let s = crate::intrinsics::to_string(vm, this)?;
    new_iterator(vm, s, 3, Root::StringIteratorPrototype)
}

/// An iterator result object `{value, done}`.
pub(crate) fn iter_result(vm: &mut Vm, value: Value, done: bool) -> Result<Value> {
    let o = object_value(vm.new_object(object_value(vm.object_prototype)));
    let (a_value, a_done) = (vm.a_value, vm.intern("done"));
    vm.create_data_property(o, Key::Name(a_value), value)?;
    vm.create_data_property(o, Key::Name(a_done), Value::from_bool(done))?;
    Ok(o)
}

fn iterator_state(this: Value, what: &str) -> Result<Value> {
    if this.is_cell() && unsafe { (*as_object(this)).class } == Class::ArrayIterator {
        return Ok(Value::from_bits(unsafe { (*as_object(this)).code } as u64));
    }
    Err(Error::type_error(format!("{what} called on an incompatible receiver")))
}

fn array_iterator_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = iterator_state(this, "%ArrayIteratorPrototype%.next")?;
    let target = vm.get_keyed(state, Value::from_i32(0))?;
    if target.is_undefined() {
        return iter_result(vm, Value::UNDEFINED, true);
    }
    let i = vm.get_keyed(state, Value::from_i32(1))?.as_number().unwrap_or(0.0);
    let kind = vm.get_keyed(state, Value::from_i32(2))?.as_number().unwrap_or(1.0) as i32;
    let o = as_object(target);
    let len = if unsafe { (*o).class } == Class::TypedArray {
        match crate::typed::ta_length(o) {
            Some(n) => n as f64,
            None => return Err(Error::type_error("Cannot perform %ArrayIteratorPrototype%.next on a detached or out of bounds typed array")),
        }
    } else {
        vm.length_of(target)?
    };
    if i >= len {
        vm.set_keyed(state, Value::from_i32(0), Value::UNDEFINED)?;
        return iter_result(vm, Value::UNDEFINED, true);
    }
    vm.set_keyed(state, Value::from_i32(1), Value::number(i + 1.0))?;
    let k = Value::number(i);
    let v = match kind {
        0 => k,
        1 => vm.get_keyed(target, k)?,
        _ => {
            let e = vm.get_keyed(target, k)?;
            crate::array::new_array_of(vm, &[k, e])?
        }
    };
    iter_result(vm, v, false)
}

fn string_iterator_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let state = iterator_state(this, "%StringIteratorPrototype%.next")?;
    let target = vm.get_keyed(state, Value::from_i32(0))?;
    let Some(u) = string_units(target) else {
        return iter_result(vm, Value::UNDEFINED, true);
    };
    let i = vm.get_keyed(state, Value::from_i32(1))?.as_number().unwrap_or(0.0) as usize;
    if i >= u.len() {
        vm.set_keyed(state, Value::from_i32(0), Value::UNDEFINED)?;
        return iter_result(vm, Value::UNDEFINED, true);
    }
    let c = u[i];
    let n = if (0xD800..0xDC00).contains(&c) && u.get(i + 1).is_some_and(|d| (0xDC00..0xE000).contains(d)) { 2 } else { 1 };
    let piece: Vec<u16> = u[i..i + n].to_vec();
    vm.set_keyed(state, Value::from_i32(1), Value::from_i32((i + n) as i32))?;
    let s = vm.new_string(&piece);
    iter_result(vm, s, false)
}

// ---- The protocol ----

/// GetIterator(v): `v[Symbol.iterator]()`, an object.
pub(crate) fn get_iterator(vm: &mut Vm, v: Value) -> Result<Value> {
    if v.is_undefined() || v.is_null() {
        return Err(Error::type_error(format!("{} is not iterable", crate::vm::display(v))));
    }
    let a = vm.a_sym_iterator;
    let m = vm.get(v, a)?;
    if !is_callable(m) {
        return Err(Error::type_error(format!("{} is not iterable", crate::vm::display(v))));
    }
    let it = call_function(vm, m, v, &[])?;
    if !is_object(it) {
        return Err(Error::type_error("Result of the Symbol.iterator method is not an object"));
    }
    Ok(it)
}

/// IteratorStep: the next value, or `None` when done.
pub(crate) fn step(vm: &mut Vm, it: Value) -> Result<Option<Value>> {
    let a_next = vm.intern("next");
    let next = vm.get(it, a_next)?;
    if !is_callable(next) {
        return Err(Error::type_error("iterator.next is not a function"));
    }
    let r = call_function(vm, next, it, &[])?;
    if !is_object(r) {
        return Err(Error::type_error(format!("Iterator result {} is not an object", crate::vm::display(r))));
    }
    let a_done = vm.intern("done");
    if to_boolean(vm.get(r, a_done)?) {
        return Ok(None);
    }
    let a_value = vm.a_value;
    vm.get(r, a_value).map(Some)
}

/// IteratorClose after an early exit: `it.return()`, if it has one.
pub(crate) fn close(vm: &mut Vm, it: Value) -> Result<()> {
    if !is_object(it) {
        return Ok(());
    }
    let a_return = vm.intern("return");
    let m = vm.get(it, a_return)?;
    if m.is_undefined() || m.is_null() {
        return Ok(());
    }
    if !is_callable(m) {
        return Err(Error::type_error("iterator.return is not a function"));
    }
    let r = call_function(vm, m, it, &[])?;
    if !is_object(r) {
        return Err(Error::type_error("Iterator result is not an object"));
    }
    Ok(())
}

/// Each value `v` iterates over, to `f` (the iterator closed if `f`
/// fails).
pub(crate) fn iterate(vm: &mut Vm, v: Value, mut f: impl FnMut(&mut Vm, Value) -> Result<()>) -> Result<()> {
    // (`it` is rooted on the native stack.)
    let it = get_iterator(vm, v)?;
    while let Some(x) = step(vm, it)? {
        if let Err(e) = f(vm, x) {
            let _ = close(vm, it);
            return Err(e);
        }
    }
    Ok(())
}

/// `Object.prototype.toString`'s tag for `o` from `@@toStringTag`, if a
/// string.
pub(crate) fn tag_of(vm: &mut Vm, o: Value) -> Option<Vec<u16>> {
    let a = vm.a_sym_to_string_tag;
    let t = vm.get(o, a).ok()?;
    string_units(t).map(|u| u.to_vec())
}
