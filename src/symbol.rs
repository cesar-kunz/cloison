//! Symbols, and the iteration protocol built on `Symbol.iterator`.
//!
//! A symbol is a cell of class `Symbol` (a primitive to JS: `typeof` is
//! "symbol", `is_object` false), whose prototype is `Symbol.prototype`.
//! As a property key it is an atom of its own, named `\u{FDD0}` and a
//! number (or the well-known symbol's name), which string keys don't
//! reach in practice; `own_keys` leaves those out (see
//! [`is_symbol_atom`]). The cell keeps its atom in `elems_len` (made
//! once it is a key: see [`symbol_atom`]) and its description in `code`'s
//! bits (traced: see `gc`).

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
    "dispose",
    "asyncDispose",
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

/// A symbol's `elems_len` until it is a property key: no atom yet.
pub(crate) const NO_ATOM: u32 = u32::MAX;

/// Symbol `v`'s property key, made the first time it is one: then its
/// atom's symbol (`Vm::symbols`, which keeps it), as `own_keys` gives it.
/// (Most symbols never are: made for each, at a million `Symbol()`s
/// atoms and symbols kept for good took 215 MB.)
pub(crate) fn symbol_atom(vm: &mut Vm, v: Value) -> Atom {
    let o = as_object(v);
    let a = unsafe { (*o).elems_len };
    if a != NO_ATOM {
        return Atom(a);
    }
    vm.symbol_count += 1;
    let atom = vm.intern_name(&format!("{SYMBOL_MARK}{}", vm.symbol_count));
    unsafe { (*o).elems_len = atom.0 };
    vm.symbols.insert(atom, o);
    atom
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

/// Whether symbol `v` is in `Symbol.for`'s registry.
pub(crate) fn is_registered(v: Value) -> bool {
    unsafe { (*as_object(v)).flags & crate::object::REGISTERED_SYMBOL != 0 }
}

/// A symbol's description (`undefined` or a string).
pub(crate) fn description(v: Value) -> Value {
    Value::from_bits(unsafe { (*as_object(v)).code } as u64)
}

/// A new symbol, its key atom named `name` (a new one if `None`).
fn make_symbol(vm: &mut Vm, desc: Value, name: Option<String>) -> Value {
    // (A well-known symbol is every realm's.)
    if let Some(n) = &name
        && let a = vm.intern_name(n)
        && let Some(&o) = vm.symbols.get(&a)
    {
        return object_value(o);
    }
    let atom = name.map(|n| vm.intern_name(&n));
    let proto = object_value(root(vm, Root::SymbolPrototype));
    let o = vm.new_object_with(Class::Symbol, proto, 0);
    unsafe {
        (*o).code = desc.to_bits() as *const crate::vm::Code;
        (*o).elems_len = atom.map_or(NO_ATOM, |a| a.0);
        // (Its class's root shape takes no element store: see
        // `Vm::root_shape`. Not a unique shape of its own, which would
        // never be freed.)
        (*o).flags |= crate::object::NOT_EXTENSIBLE;
    }
    if let Some(a) = atom {
        vm.symbols.insert(a, o);
    }
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

/// [`symbol_string`] as a string value: a RangeError past the longest
/// string, thrown before a long description is read.
pub(crate) fn symbol_string_value(vm: &mut Vm, v: Value) -> Result<Value> {
    crate::vm::check_string_len(crate::vm::string_len(description(v)).unwrap_or(0) + 8)?;
    let u = symbol_string(v);
    Ok(vm.new_string(&u))
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
    // (A root of every collection from now on: the registry's symbols live
    // as long as the agent, GlobalSymbolRegistry. Full collections
    // enumerate it, `Vm::registered_symbols`; the log has it for the next
    // minor one.)
    unsafe { (*as_object(s)).flags |= crate::object::REGISTERED_SYMBOL };
    vm.heap.note(s);
    vm.symbol_registry.insert(units, as_object(s));
    Ok(s)
}

fn symbol_key_for(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let s = arg(a, 0);
    if !is_symbol(s) {
        return Err(Error::type_error(format!("{} is not a symbol", crate::vm::shown(s))));
    }
    Ok(if is_registered(s) { description(s) } else { Value::UNDEFINED })
}

pub(crate) fn symbol_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let s = this_symbol(this, "Symbol.prototype.toString")?;
    symbol_string_value(vm, s)
}

pub(crate) fn symbol_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
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
    let keys: Vec<Atom> = unsafe { (*(*o).shape).keys().to_vec() };
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

/// `Function.prototype[Symbol.hasInstance](v)`: OrdinaryHasInstance.
fn function_has_instance(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let v = a.first().copied().unwrap_or(Value::UNDEFINED);
    vm.ordinary_has_instance(v, this).map(Value::from_bool)
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
        let a = vm.intern_name(w);
        let d = Desc::data(s, false, false, false);
        let _ = vm.define_own_property(ctor, Key::Name(a), &d);
    }
    vm.a_sym_iterator = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.iterator"));
    vm.a_sym_species = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.species"));
    vm.a_sym_to_string_tag = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.toStringTag"));
    vm.a_sym_to_primitive = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.toPrimitive"));
    vm.a_sym_has_instance = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.hasInstance"));
    // `Function.prototype[Symbol.hasInstance]`: fixed.
    let hi = vm.native_function("[Symbol.hasInstance]", function_has_instance, 1, crate::jit::Ty::Any, false);
    vm.has_instance_fn = hi;
    let d = Desc::data(object_value(hi), false, false, false);
    let (fp, a_hi) = (vm.function_prototype, vm.a_sym_has_instance);
    let _ = vm.define_own_property(fp, Key::Name(a_hi), &d);
    // `Date.prototype[Symbol.toPrimitive]`.
    let dtp = vm.native_function("[Symbol.toPrimitive]", crate::date::date_to_primitive, 1, crate::jit::Ty::Any, false);
    let d = Desc::data(object_value(dtp), false, false, true);
    let (dp, a_tp) = (vm.date_prototype, vm.intern_name(&format!("{SYMBOL_MARK}Symbol.toPrimitive")));
    let _ = vm.define_own_property(dp, Key::Name(a_tp), &d);
    // `Array.prototype[Symbol.unscopables]`: what `with` over an array
    // doesn't find (ES2015 on).
    let u = vm.new_object(Value::NULL);
    for name in [
        "at",
        "copyWithin",
        "entries",
        "fill",
        "find",
        "findIndex",
        "findLast",
        "findLastIndex",
        "flat",
        "flatMap",
        "includes",
        "keys",
        "toReversed",
        "toSorted",
        "toSpliced",
        "values",
    ] {
        let a = vm.intern_name(name);
        vm.add_property_with(u, a, Value::TRUE, 0);
    }
    let a_unscopables = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.unscopables"));
    let d = Desc::data(object_value(u), false, false, true);
    let ap = vm.array_prototype;
    let _ = vm.define_own_property(ap, Key::Name(a_unscopables), &d);
    vm.define_natives(ctor, &[("for", symbol_for as NativeFn, 1), ("keyFor", symbol_key_for, 1)]);
    vm.define_natives(sp, &[("toString", symbol_to_string as NativeFn, 0), ("valueOf", symbol_value_of, 0)]);
    let a_description = vm.intern_name("description");
    define_getter(vm, sp, Key::Name(a_description), "description", symbol_description);
    let tp = vm.a_sym_to_primitive;
    let f = vm.native_function("[Symbol.toPrimitive]", symbol_value_of, 1, crate::jit::Ty::Any, false);
    let d = Desc::data(object_value(f), false, false, true);
    let _ = vm.define_own_property(sp, Key::Name(tp), &d);
    to_string_tag(vm, sp, "Symbol");
    let object_ctor = {
        let a = vm.intern_name("Object");
        vm.get(object_value(g), a)
    }
    .unwrap_or(Value::UNDEFINED);
    if object_ctor.is_cell() {
        vm.define_natives(
            as_object(object_ctor),
            &[("getOwnPropertySymbols", get_own_property_symbols as NativeFn, 1)],
        );
    }

    // `@@species` getters: the constructor itself.
    let a_species = vm.intern_name(&format!("{SYMBOL_MARK}Symbol.species"));
    for name in ["Array", "Map", "Set", "Promise", "RegExp", "ArrayBuffer", "SharedArrayBuffer"] {
        let a = vm.intern_name(name);
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
        let a = vm.intern_name(method);
        let f = vm.get(object_value(o), a).unwrap_or(Value::UNDEFINED);
        vm.add_property_with(o, a_iter, f, DONT_ENUM);
        to_string_tag(vm, o, tag);
    }
    for (p, tag) in [(Proto::MapIterator, "Map Iterator"), (Proto::SetIterator, "Set Iterator")] {
        let o = cp[p as usize];
        vm.set_prototype_of(o, object_value(ip));
        to_string_tag(vm, o, tag);
    }
    crate::promise::install_symbols(vm);
    crate::module::install_symbols(vm);
    to_string_tag(vm, cp[Proto::WeakMap as usize], "WeakMap");
    to_string_tag(vm, cp[Proto::WeakSet as usize], "WeakSet");
    to_string_tag(vm, cp[Proto::WeakRef as usize], "WeakRef");
    to_string_tag(vm, cp[Proto::FinalizationRegistry as usize], "FinalizationRegistry");
    to_string_tag(vm, cp[Proto::DataView as usize], "DataView");
    let math = {
        let a = vm.intern_name("Math");
        vm.get(object_value(g), a)
    }
    .unwrap_or(Value::UNDEFINED);
    if is_object(math) {
        to_string_tag(vm, as_object(math), "Math");
    }
    for name in ["JSON", "Reflect"] {
        let o = {
            let a = vm.intern_name(name);
            vm.get(object_value(g), a)
        }
        .unwrap_or(Value::UNDEFINED);
        if is_object(o) {
            to_string_tag(vm, as_object(o), name);
        }
    }
}

pub(crate) fn return_this(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
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
    // The shape the realm's first one got: given at once, the two slots
    // stored (V8's iterator result map, JSC's iteratorResultObjectStructure).
    let s = vm.iter_result_shape;
    if !s.is_null() {
        unsafe {
            crate::object::mark_shape(s);
            let o = vm.heap.alloc_object(&*s);
            vm.store_field(o, 0, value);
            vm.store_field(o, 1, Value::from_bool(done));
            return Ok(object_value(o));
        }
    }
    let o = object_value(vm.new_object(object_value(vm.object_prototype)));
    let (a_value, a_done) = (vm.a_value, vm.a_done);
    vm.create_data_property(o, Key::Name(a_value), value)?;
    vm.create_data_property(o, Key::Name(a_done), Value::from_bool(done))?;
    unsafe {
        let s = (*as_object(o)).shape;
        if !(*s).dictionary && (*s).keys() == [a_value, a_done] && (*s).attrs().iter().all(|&a| a == 0) {
            // (A realm's field: `Vm::fixed_shapes` keeps it, and the
            // collector its ancestors.)
            vm.iter_result_shape = s;
        }
    }
    Ok(o)
}

fn iterator_state(this: Value, what: &str) -> Result<Value> {
    if this.is_cell() && unsafe { (*as_object(this)).class } == Class::ArrayIterator {
        return Ok(Value::from_bits(unsafe { (*as_object(this)).code } as u64));
    }
    Err(Error::type_error(format!("{what} called on an incompatible receiver")))
}

fn array_iterator_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    match array_step(vm, this)? {
        Some(v) => iter_result(vm, v, false),
        None => iter_result(vm, Value::UNDEFINED, true),
    }
}

/// `%ArrayIteratorPrototype%.next` on `this`, without its result object:
/// the value, or `None` when done.
fn array_step(vm: &mut Vm, this: Value) -> Result<Option<Value>> {
    let state = iterator_state(this, "%ArrayIteratorPrototype%.next")?;
    // Values of a dense array (`[target, index, kind]` itself one, plain):
    // its element read directly, unless a hole (a prototype's then).
    unsafe {
        let st = as_object(state);
        if (*st).class == Class::Array && (*st).flags & crate::far::FAR == 0 && (*st).elems_len == 3 {
            let e = (*st).elems;
            let (target, i, kind) = (*e, *e.add(1), *e.add(2));
            if target.is_cell() && i.is_int32() && kind == Value::from_i32(1) {
                let t = as_object(target);
                let n = i.as_i32_unchecked();
                if (*t).class == Class::Array && (*t).flags & crate::far::FAR == 0 && (0..i32::MAX).contains(&n) {
                    if n as u32 >= (*t).elems_len {
                        vm.store_element(st, 0, Value::UNDEFINED);
                        return Ok(None);
                    }
                    let v = *(*t).elems.add(n as usize);
                    if v != Value::EMPTY && v != Value::HOLE {
                        vm.store_element(st, 1, Value::from_i32(n + 1));
                        return Ok(Some(v));
                    }
                }
            }
        }
    }
    let target = vm.get_keyed(state, Value::from_i32(0))?;
    if target.is_undefined() {
        return Ok(None);
    }
    let i = vm.get_keyed(state, Value::from_i32(1))?.as_number().unwrap_or(0.0);
    let kind = vm.get_keyed(state, Value::from_i32(2))?.as_number().unwrap_or(1.0) as i32;
    let o = as_object(target);
    let len = if unsafe { (*o).class } == Class::TypedArray {
        // (Its record made seq-cst: 23.1.5.1.)
        match crate::typed::ta_length_seq_cst(vm, o) {
            Some(n) => n as f64,
            None => {
                return Err(Error::type_error(
                    "Cannot perform %ArrayIteratorPrototype%.next on a detached or out of bounds typed array",
                ));
            }
        }
    } else {
        vm.length_of(target)?
    };
    if i >= len {
        vm.set_keyed(state, Value::from_i32(0), Value::UNDEFINED)?;
        return Ok(None);
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
    Ok(Some(v))
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
    let n =
        if (0xD800..0xDC00).contains(&c) && u.get(i + 1).is_some_and(|d| (0xDC00..0xE000).contains(d)) { 2 } else { 1 };
    let piece: Vec<u16> = u[i..i + n].to_vec();
    vm.set_keyed(state, Value::from_i32(1), Value::from_i32((i + n) as i32))?;
    let s = vm.new_string(&piece);
    iter_result(vm, s, false)
}

// ---- The protocol ----

/// GetIterator(v): `v[Symbol.iterator]()`, an object.
pub(crate) fn get_iterator(vm: &mut Vm, v: Value) -> Result<Value> {
    if v.is_undefined() || v.is_null() {
        return Err(Error::type_error(format!("{} is not iterable", crate::vm::shown(v))));
    }
    let a = vm.a_sym_iterator;
    let m = vm.get(v, a)?;
    if !is_callable(m) {
        return Err(Error::type_error(format!("{} is not iterable", crate::vm::shown(v))));
    }
    let it = call_function(vm, m, v, &[])?;
    if !is_object(it) {
        return Err(Error::type_error("Result of the Symbol.iterator method is not an object"));
    }
    Ok(it)
}

/// An iterator record for bytecode, which keeps it in a register: the
/// iterator and its `next`, read once as GetIterator does, in the two
/// in-object slots of an object no JS code ever sees (no properties, the
/// collector tracing the slots by its capacity).
pub(crate) fn record(vm: &mut Vm, it: Value) -> Result<Value> {
    record_with(vm, it, None)
}

/// A [`record`] of sync iterator `it` stepped as AsyncFromSyncIterator
/// (`for await`, `yield*`): a third slot, whether its last step was done
/// ([`set_sync_done`]).
pub(crate) fn sync_record(vm: &mut Vm, it: Value) -> Result<Value> {
    record_with(vm, it, Some(Value::FALSE))
}

/// A [`record`] of async iterator `it`: a third slot, whether `yield*`'s
/// last step had no method for its `throw` (the iterator closed instead)
/// or `return` (kept as a [`sync_record`]'s `done`: [`set_sync_done`]).
pub(crate) fn async_record(vm: &mut Vm, it: Value) -> Result<Value> {
    record_with(vm, it, Some(Value::FALSE))
}

fn record_with(vm: &mut Vm, it: Value, done: Option<Value>) -> Result<Value> {
    let a_next = vm.a_next;
    let next = vm.get(it, a_next)?;
    let o = vm.new_object_with(crate::object::Class::Plain, Value::NULL, 2 + done.is_some() as u32);
    for (i, v) in [it, next].into_iter().chain(done).enumerate() {
        unsafe {
            let at = crate::object::Object::inline(o, i);
            crate::heap::store(o.cast(), at, v);
        }
    }
    Ok(crate::object::object_value(o))
}

/// Whether a [`sync_record`]'s last step was done (false once
/// [`set_sync_result`]).
pub(crate) fn sync_done(r: Value) -> bool {
    unsafe { *crate::object::Object::inline(crate::object::as_object(r), 2) == Value::TRUE }
}

pub(crate) fn set_sync_done(r: Value, done: bool) {
    // (Not a cell: no barrier.)
    unsafe {
        crate::layout::store_word(crate::object::Object::inline(crate::object::as_object(r), 2), Value::from_bool(done))
    }
}

/// Mark a [`sync_record`]'s last step's promise one of an iterator
/// result, not of its value: what a callable `Object.prototype.then`
/// gave for the one AsyncFromSyncIterator resolved it with
/// (`promise::resolve_iter_result`), for its consumer to read as the
/// spec's do. (Null in the `done` slot, till the next step's.)
pub(crate) fn set_sync_result(r: Value) {
    unsafe { crate::layout::store_word(crate::object::Object::inline(crate::object::as_object(r), 2), Value::NULL) }
}

/// Whether a [`sync_record`]'s last step was [`set_sync_result`].
pub(crate) fn sync_result(r: Value) -> bool {
    unsafe { *crate::object::Object::inline(crate::object::as_object(r), 2) == Value::NULL }
}

/// A [`record`]'s iterator and `next`.
pub(crate) fn record_parts(r: Value) -> (Value, Value) {
    let o = crate::object::as_object(r);
    unsafe { (*crate::object::Object::inline(o, 0), *crate::object::Object::inline(o, 1)) }
}

/// IteratorStep of `it` with its `next`: the next value, or `None` when
/// done.
pub(crate) fn step_with(vm: &mut Vm, it: Value, next: Value) -> Result<Option<Value>> {
    if !is_callable(next) {
        return Err(Error::type_error("iterator.next is not a function"));
    }
    // (The builtin array iterator's step, its result object unobservable:
    // fresh, its own data properties read back at once.)
    if is_native(vm, next, array_iterator_next)
        && it.is_cell()
        && unsafe { (*as_object(it)).class } == Class::ArrayIterator
    {
        return array_step(vm, it);
    }
    let r = call_function(vm, next, it, &[])?;
    if !is_object(r) {
        return Err(Error::type_error(format!("Iterator result {} is not an object", crate::vm::shown(r))));
    }
    let a_done = vm.a_done;
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
    let a_return = vm.a_return;
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
    // An iteration by the builtins: its values as `next()` gives them,
    // with no result objects (see `IteratorOpen`). (The state on the
    // native stack, which pins what it holds.)
    let mut st = [v, Value::UNDEFINED];
    if unsafe { fast_open(vm, st.as_mut_ptr()) } {
        loop {
            let x = unsafe { iterator_next(vm, st.as_mut_ptr())? };
            if x == Value::HOLE {
                return Ok(());
            }
            if let Err(e) = f(vm, x) {
                let _ = open_state_close(vm, &st);
                return Err(e);
            }
        }
    }
    // (`it` and `next` are rooted on the native stack.)
    let it = get_iterator(vm, v)?;
    let a_next = vm.a_next;
    let next = vm.get(it, a_next)?;
    while let Some(x) = step_with(vm, it, next)? {
        if let Err(e) = f(vm, x) {
            let _ = close(vm, it);
            return Err(e);
        }
    }
    Ok(())
}

/// `Object.prototype.toString`'s tag for `o` from `@@toStringTag`, if a
/// string (what its getter throws thrown).
pub(crate) fn tag_of(vm: &mut Vm, o: Value) -> Result<Option<Vec<u16>>> {
    let a = vm.a_sym_to_string_tag;
    let t = vm.get(o, a)?;
    Ok(string_units(t).map(|u| u.to_vec()))
}

/// Whether `f` is the current realm's native `n` (by its code's
/// function pointer, and its code's realm). What a fast path stands in
/// for must be: a builtin runs in its own realm (§10.3.1), and what it
/// makes (an iterator, an entry's array) has that realm's prototypes.
fn is_native(vm: &Vm, f: Value, n: NativeFn) -> bool {
    f.is_cell()
        && unsafe { (*as_object(f)).class } == Class::Native
        && unsafe {
            let c = (*as_object(f)).code;
            !c.is_null() && (*c).native.is_some_and(|g| g as usize == n as usize) && (*c).realm == vm.realm
        }
}

/// The iteration protector's bits: arrays iterate by the builtins
/// (`Array.prototype[@@iterator]` the builtin `values`,
/// `%ArrayIteratorPrototype%.next` the builtin `next`, data properties
/// both)...
pub(crate) const ITER_ARRAY: u8 = 1;
/// ...and no iterator prototype [`fast_open`] uses has `return`, nor
/// what they inherit from...
pub(crate) const ITER_NO_RETURN: u8 = 2;
/// ...maps iterate by the builtins (`Map.prototype[@@iterator]` the
/// builtin `entries`, `%MapIteratorPrototype%.next` the builtin)...
pub(crate) const ITER_MAP: u8 = 4;
/// ...sets (`values` and `%SetIteratorPrototype%.next`)...
pub(crate) const ITER_SET: u8 = 8;
/// ...and strings (`String.prototype[@@iterator]` and
/// `%StringIteratorPrototype%.next`).
pub(crate) const ITER_STRING: u8 = 16;

/// The iteration protector, V8's `ArrayIteratorLookupChain` (or
/// JavaScriptCore's array iterator protocol watchpoint): bits that hold
/// while the prototype epoch does. What they rest on is on prototypes,
/// which `make_prototype` gives unique shapes, so adding or deleting a
/// property of one bumps the epoch; the slots they read are in
/// `relied_slots`, so storing another value in one bumps it too.
#[inline]
pub(crate) fn protector(vm: &mut Vm) -> u8 {
    if vm.iter_epoch == Some(vm.proto_epoch) {
        return vm.iter_bits;
    }
    protector_again(vm)
}

/// [`protector`]'s bits computed again, the epoch having moved.
#[cold]
#[inline(never)]
fn protector_again(vm: &mut Vm) -> u8 {
    use crate::collections::Proto;
    let aip = root(vm, Root::ArrayIteratorPrototype);
    let sip = root(vm, Root::StringIteratorPrototype);
    let cp = |p: Proto| vm.collection_prototypes[p as usize];
    let (mp, sp, mip, setip) = (cp(Proto::Map), cp(Proto::Set), cp(Proto::MapIterator), cp(Proto::SetIterator));
    let iterators = [aip, sip, mip, setip];
    // (Each of the chains, prototypes: a change to any bumps the epoch.
    // A proxy on one may run code for any lookup, which `Vm::lookup`
    // doesn't: then no bits.)
    let mut proxied = false;
    for start in [vm.array_prototype, vm.string_prototype, mp, sp].into_iter().chain(iterators) {
        let mut o = start;
        while !o.is_null() {
            proxied |= crate::proxy::is_proxy(vm, o);
            vm.make_prototype(o);
            let p = unsafe { (*(*o).shape).proto };
            o = if p.is_cell() { as_object(p) } else { std::ptr::null_mut() };
        }
    }
    let rely = |vm: &mut Vm, o: *mut Object, name: Atom| -> Option<Value> {
        let (h, slot) = vm.lookup(o, name)?;
        if vm.is_accessor(h, slot) {
            return None;
        }
        let at = unsafe { Object::slot(h, (*(*h).shape).location(slot)) };
        vm.relied_slots.insert(at as usize);
        Some(unsafe { *at })
    };
    let values = object_value(root(vm, Root::ArrayValues));
    let mut bits = 0;
    // (`is_native`: the current realm's own builtins, factories and
    // `next`s alike.)
    let builtins =
        |vm: &mut Vm, proto: *mut Object, method: Option<NativeFn>, it_proto: *mut Object, next: NativeFn| {
            let m = rely(vm, proto, vm.a_sym_iterator);
            let ok = match method {
                Some(f) => m.is_some_and(|m| is_native(vm, m, f)),
                None => m == Some(values),
            };
            ok && rely(vm, it_proto, vm.a_next).is_some_and(|n| is_native(vm, n, next))
        };
    if builtins(vm, vm.array_prototype, None, aip, array_iterator_next) {
        bits |= ITER_ARRAY;
    }
    if builtins(vm, mp, Some(crate::collections::map_entries), mip, crate::collections::iterator_next) {
        bits |= ITER_MAP;
    }
    if builtins(vm, sp, Some(crate::collections::set_values), setip, crate::collections::iterator_next) {
        bits |= ITER_SET;
    }
    if builtins(vm, vm.string_prototype, Some(string_iterator), sip, string_iterator_next) {
        bits |= ITER_STRING;
    }
    if iterators.iter().all(|&p| vm.lookup(p, vm.a_return).is_none()) {
        bits |= ITER_NO_RETURN;
    }
    if proxied {
        bits = 0;
    }
    vm.iter_bits = bits;
    vm.iter_epoch = Some(vm.proto_epoch);
    bits
}

/// Whether `v` iterates as an array by the builtins: an array (not a
/// proxy) with `Array.prototype` for its prototype and no own
/// `@@iterator`, while the [`protector`] has [`ITER_ARRAY`]. Nothing of its iteration is observable then but the
/// reads of its length and its elements, which `IteratorNext` does as
/// `next()` would.
#[inline]
fn iterates_as_array(vm: &mut Vm, v: Value) -> bool {
    if !v.is_cell() || unsafe { (*as_object(v)).class } != Class::Array {
        return false;
    }
    let o = as_object(v);
    if unsafe { (*(*o).shape).proto } != object_value(vm.array_prototype) {
        return false;
    }
    if unsafe { (*(*o).shape).lookup(vm.a_sym_iterator).is_some() } {
        return false;
    }
    protector(vm) & ITER_ARRAY != 0
}

/// `IteratorOpen`'s states with no iterator record, tried in
/// `slots[0..2]` (the iterable in `slots[0]`): an array iterated by the
/// builtins ([`iterates_as_array`]) and its next index; a string and its
/// next index; a map's or a set's iterator, made as its builtin
/// `@@iterator` makes it (stepped without result objects); whether one
/// was. Nothing observable is skipped: the protector stands for the
/// lookups, and the iterator objects made are never seen.
#[inline]
pub(crate) unsafe fn fast_open(vm: &mut Vm, slots: *mut Value) -> bool {
    unsafe {
        if iterates_as_array(vm, *slots) {
            *slots.add(1) = Value::from_i32(0);
            return true;
        }
        fast_open_other(vm, slots)
    }
}

/// [`fast_open`]'s strings, maps and sets.
#[inline(never)]
unsafe fn fast_open_other(vm: &mut Vm, slots: *mut Value) -> bool {
    unsafe {
        let v = *slots;
        if !v.is_cell() {
            return false;
        }
        let o = as_object(v);
        let (bit, proto) = match (*o).class {
            Class::String => {
                if protector(vm) & ITER_STRING == 0 {
                    return false;
                }
                *slots.add(1) = Value::from_i32(0);
                return true;
            }
            Class::Map => (ITER_MAP, crate::collections::Proto::Map),
            Class::Set => (ITER_SET, crate::collections::Proto::Set),
            _ => return false,
        };
        if (*(*o).shape).proto != object_value(vm.collection_prototypes[proto as usize])
            || (*(*o).shape).lookup(vm.a_sym_iterator).is_some()
            || protector(vm) & bit == 0
        {
            return false;
        }
        *slots = crate::collections::default_iterator(vm, o);
        true
    }
}

/// `IteratorOpen`: the iterable in `slots[0]`, then the iteration's
/// state in `slots[0..2]`: one of [`fast_open`]'s, else a [`record`] of
/// its iterator. (The slots are the frame's registers, read after any
/// safepoint: a collection moving cells updates them.)
pub(crate) unsafe fn iterator_open(vm: &mut Vm, slots: *mut Value) -> Result<()> {
    unsafe {
        if fast_open(vm, slots) {
            return Ok(());
        }
        let it = get_iterator(vm, *slots)?;
        let r = record(vm, it)?;
        *slots = r;
        *slots.add(1) = Value::UNDEFINED;
        Ok(())
    }
}

/// Whether `IteratorOpen`'s state starting at `v` is one of
/// [`fast_open`]'s (a record is a plain object).
pub(crate) fn is_open_state(v: Value) -> bool {
    v.is_cell() && matches!(unsafe { (*as_object(v)).class }, Class::Array | Class::String | Class::CollectionIterator)
}

/// `src`'s elements appended to array `dst`, as iterating `src` would
/// give them, copied at once when `src` is an array iterated by the
/// builtins with no holes and `dst` a plain array (V8's
/// `CloneFastJSArray`): whether it was. Nothing collects meanwhile
/// (allocation never does), so the elements are read in place.
pub(crate) fn append_dense(vm: &mut Vm, dst: Value, src: Value) -> bool {
    if !iterates_as_array(vm, src) || !is_open_array(dst) {
        return false;
    }
    let (d, s) = (as_object(dst), as_object(src));
    unsafe {
        if (*d).flags & !(crate::object::EXTERNAL | crate::object::PROTOTYPE) != 0
            || crate::far::is_far(d)
            || crate::far::is_far(s)
        {
            return false;
        }
        let n = (*s).elems_len as usize;
        if vm.length_field(s) as usize != n || (0..n).any(|i| *(*s).elems.add(i) == Value::EMPTY) {
            return false;
        }
        let len = (*d).elems_len as usize;
        vm.reserve_elems(d, len + n);
        vm.store_elements(d, len, (*s).elems, n);
        (*d).elems_len = (len + n) as u32;
    }
    true
}

/// Whether `v`, the first register of an `IteratorOpen` state, makes it
/// an array's (`[array, index]`).
pub(crate) fn is_open_array(v: Value) -> bool {
    v.is_cell() && unsafe { (*as_object(v)).class } == Class::Array
}

/// Element `i` of array `a` as `%ArrayIteratorPrototype%.next` gives it,
/// `None` past its length.
pub(crate) fn open_array_element(vm: &mut Vm, a: Value, i: u32) -> Result<Option<Value>> {
    let o = as_object(a);
    if i >= vm.length_field(o) {
        return Ok(None);
    }
    unsafe {
        if i < (*o).elems_len {
            let v = *(*o).elems.add(i as usize);
            if v != Value::EMPTY {
                return Ok(Some(v));
            }
        }
    }
    vm.get_keyed(a, Value::number(i as f64)).map(Some)
}

/// The next index in an array's `IteratorOpen` state (under 2^32: an
/// array's indices are).
pub(crate) fn open_index(v: Value) -> u32 {
    v.as_number().expect("an index") as u32
}

/// `IteratorNext`: the next value of the iteration in `slots[0..2]`, or
/// `HOLE` when done (an array's state then `undefined`, as its
/// iterator's target is once done).
#[inline]
pub(crate) unsafe fn iterator_next(vm: &mut Vm, slots: *mut Value) -> Result<Value> {
    unsafe {
        let s = *slots;
        if s.is_undefined() {
            return Ok(Value::HOLE);
        }
        if is_open_array(s) {
            // (A number: a far array's indices go past an int32's.)
            let i = open_index(*slots.add(1));
            *slots.add(1) = Value::number(i as f64 + 1.0);
            return Ok(match open_array_element(vm, s, i)? {
                Some(v) => v,
                None => {
                    *slots = Value::UNDEFINED;
                    Value::HOLE
                }
            });
        }
        iterator_next_other(vm, slots)
    }
}

/// [`iterator_next`] of a string's, a map's or a set's state, or a
/// record's.
#[inline(never)]
unsafe fn iterator_next_other(vm: &mut Vm, slots: *mut Value) -> Result<Value> {
    unsafe {
        let s = *slots;
        match (*as_object(s)).class {
            Class::String => {
                let i = (*slots.add(1)).as_i32_unchecked() as usize;
                let u = string_units(s).expect("a string");
                if i >= u.len() {
                    *slots = Value::UNDEFINED;
                    return Ok(Value::HOLE);
                }
                let c = u[i];
                let pair = (0xD800..0xDC00).contains(&c) && u.get(i + 1).is_some_and(|d| (0xDC00..0xE000).contains(d));
                *slots.add(1) = Value::from_i32((i + 1 + pair as usize) as i32);
                return Ok(if pair {
                    let piece = [c, u[i + 1]];
                    vm.new_string(&piece)
                } else {
                    vm.single_char(c)
                });
            }
            Class::CollectionIterator => {
                return Ok(match crate::collections::step(vm, as_object(s))? {
                    Some(v) => v,
                    None => {
                        *slots = Value::UNDEFINED;
                        Value::HOLE
                    }
                });
            }
            _ => {}
        }
        let (it, next) = record_parts(s);
        Ok(step_with(vm, it, next)?.unwrap_or(Value::HOLE))
    }
}

/// IteratorClose of one of [`fast_open`]'s states `[v, index]`: its
/// iterator's `return`, which the iterator prototypes and what they
/// inherit from don't have unless a script gave them one; then called
/// on its iterator (an array's or a string's made for it, at that
/// index).
pub(crate) fn open_state_close(vm: &mut Vm, a: &[Value]) -> Result<()> {
    if protector(vm) & ITER_NO_RETURN != 0 {
        return Ok(());
    }
    let it = match unsafe { (*as_object(a[0])).class } {
        Class::CollectionIterator => a[0],
        Class::String => new_iterator(vm, a[0], 3, Root::StringIteratorPrototype)?,
        _ => new_iterator(vm, a[0], 1, Root::ArrayIteratorPrototype)?,
    };
    if unsafe { (*as_object(it)).class } == Class::ArrayIterator {
        let state = Value::from_bits(unsafe { (*as_object(it)).code } as u64);
        vm.set_keyed(state, Value::from_i32(1), a[1])?;
    }
    close(vm, it)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An array's `IteratorOpen` state steps past index 2^31 - 1 (a far
    /// array's): its index a number, not an int32 that wraps to -2^31.
    #[test]
    fn array_states_step_past_int32_indices() {
        let mut vm = Vm::new();
        let a = vm.new_array();
        let top = 2f64.powi(31);
        for (k, x) in [(top - 1.0, 1), (top, 2)] {
            vm.set_keyed(a, Value::number(k), Value::from_i32(x)).unwrap();
        }
        let mut st = [a, Value::number(top - 1.0)];
        let got: Vec<u64> =
            (0..3).map(|_| unsafe { iterator_next(&mut vm, st.as_mut_ptr()).unwrap() }.to_bits()).collect();
        assert_eq!(got, [Value::from_i32(1).to_bits(), Value::from_i32(2).to_bits(), Value::HOLE.to_bits()]);
        assert!(st[0].is_undefined(), "done");
    }
}
