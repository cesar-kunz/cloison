//! `Map`, `Set`, `WeakMap` and `WeakSet` (ECMA-262 §24.1–24.4), their
//! iterators, and `DataView` (§25.3). Installed by [`install`].
//!
//! A collection keeps its entries in a hidden array (`Class::Array`, which
//! JS never sees) whose value is in the object's `code` bits (traced by the
//! collector as a wrapper's primitive is): the elements are `k0, v0, k1,
//! v1, …` for maps, `k0, k1, …` for sets, in insertion order, and a
//! deleted entry's slots are holes. Lookups are linear scans by
//! SameValueZero. Adding an entry to an array more than half holes first
//! copies the live ones into a fresh array: an iteration in progress keeps
//! the old one (so misses entries added after; `clear` only empties the
//! slots, as the spec does, so iterators see it). The weak kinds hold their
//! keys strongly.

use crate::builtins::{getter, is_class, link_constructor};
use crate::interp::{call_function, to_boolean, to_int32, to_number};
use crate::jit::Ty;
use crate::object::{Class, DONT_ENUM, Object, as_object, object_value};
use crate::props::{is_callable, is_object, same_value};
use crate::value::Value;
use crate::vm::{Code, Error, NativeFn, Result, Vm, array_push, string_units};

/// Indices into `Vm::collection_prototypes`.
#[derive(Clone, Copy)]
#[repr(usize)]
enum Proto {
    Map,
    Set,
    WeakMap,
    WeakSet,
    DataView,
    MapIterator,
    SetIterator,
}

/// What an iterator yields; `elems_cap`'s low two bits.
#[derive(Clone, Copy)]
#[repr(u32)]
enum IterMode {
    Keys,
    Values,
    Entries,
}

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = Ty::Any;
    for _ in 0..7 {
        let p = vm.new_object(op);
        vm.collection_prototypes.push(p);
    }
    let protos = vm.collection_prototypes.clone();
    let ctors: [(&str, NativeFn); 4] = [("Map", map_ctor), ("Set", set_ctor), ("WeakMap", weak_map_ctor), ("WeakSet", weak_set_ctor)];
    for (k, (name, f)) in ctors.into_iter().enumerate() {
        let c = vm.define_native(g, name, f, 0, any, true);
        link_constructor(vm, c, protos[k]);
    }
    let mp = protos[Proto::Map as usize];
    vm.define_natives(
        mp,
        &[
            ("get", map_get as NativeFn, 1),
            ("set", map_set, 2),
            ("has", map_has, 1),
            ("delete", map_delete, 1),
            ("clear", map_clear, 0),
            ("forEach", map_for_each, 1),
            ("keys", map_keys, 0),
            ("values", map_values, 0),
            ("entries", map_entries, 0),
        ],
    );
    getter(vm, mp, "size", map_size);
    let sp = protos[Proto::Set as usize];
    vm.define_natives(
        sp,
        &[
            ("add", set_add as NativeFn, 1),
            ("has", set_has, 1),
            ("delete", set_delete, 1),
            ("clear", set_clear, 0),
            ("forEach", set_for_each, 1),
            ("entries", set_entries, 0),
        ],
    );
    // `keys` is the very function `values` is.
    let values = vm.define_native(sp, "values", set_values, 0, any, false);
    let a_keys = vm.intern("keys");
    vm.add_property_with(sp, a_keys, object_value(values), DONT_ENUM);
    getter(vm, sp, "size", set_size);
    vm.define_natives(
        protos[Proto::WeakMap as usize],
        &[
            ("get", weak_map_get as NativeFn, 1),
            ("set", weak_map_set, 2),
            ("has", weak_map_has, 1),
            ("delete", weak_map_delete, 1),
        ],
    );
    vm.define_natives(
        protos[Proto::WeakSet as usize],
        &[("add", weak_set_add as NativeFn, 1), ("has", weak_set_has, 1), ("delete", weak_set_delete, 1)],
    );
    vm.define_native(protos[Proto::MapIterator as usize], "next", iterator_next, 0, any, false);
    vm.define_native(protos[Proto::SetIterator as usize], "next", iterator_next, 0, any, false);

    let dp = protos[Proto::DataView as usize];
    let c = vm.define_native(g, "DataView", data_view_ctor, 1, any, true);
    link_constructor(vm, c, dp);
    getter(vm, dp, "buffer", view_buffer);
    getter(vm, dp, "byteLength", view_byte_length);
    getter(vm, dp, "byteOffset", view_byte_offset);
    const GETS: [NativeFn; 8] =
        [view_get::<0>, view_get::<1>, view_get::<2>, view_get::<3>, view_get::<4>, view_get::<5>, view_get::<6>, view_get::<7>];
    const SETS: [NativeFn; 8] =
        [view_set::<0>, view_set::<1>, view_set::<2>, view_set::<3>, view_set::<4>, view_set::<5>, view_set::<6>, view_set::<7>];
    let names: Vec<(String, NativeFn, u32)> = (0..8)
        .flat_map(|t| [(format!("get{}", VIEW_TYPES[t].0), GETS[t], 1), (format!("set{}", VIEW_TYPES[t].0), SETS[t], 2)])
        .collect();
    let fs: Vec<(&str, NativeFn, u32)> = names.iter().map(|(n, f, k)| (n.as_str(), *f, *k)).collect();
    vm.define_natives(dp, &fs);
}

// ---- Helpers ----

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// `this` as an object of class `c`, else a TypeError from `what`.
fn this_of(this: Value, c: Class, what: &str) -> Result<*mut Object> {
    if is_class(this, c) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error(format!("{what} called on incompatible receiver")))
    }
}

/// ToIndex of `v`: an integer in `0..2^53`, else a RangeError (what a
/// `valueOf` threw propagates).
fn to_index(vm: &mut Vm, v: Value) -> Result<usize> {
    if v.is_undefined() {
        return Ok(0);
    }
    let d = to_number(v);
    if vm.conv_thrown {
        return Err(vm.take_conv_error());
    }
    let d = if d.is_nan() { 0.0 } else { d.trunc() };
    if !(0.0..=9007199254740991.0).contains(&d) {
        return Err(Error::range_error(format!("Invalid index {d}")));
    }
    Ok(d as usize)
}

/// SameValueZero: SameValue, but the zeros are equal.
fn same_value_zero(a: Value, b: Value) -> bool {
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        return x == y || x.is_nan() && y.is_nan();
    }
    same_value(a, b)
}

/// `k` as a map key: -0 is +0.
fn normalize(k: Value) -> Value {
    if k.is_double() && k.as_f64_unchecked() == 0.0 { Value::from_i32(0) } else { k }
}

/// The entries array of collection (or iterator) `o`.
fn entries(o: *mut Object) -> *mut Object {
    unsafe { as_object(Value::from_bits((*o).code as u64)) }
}

/// Set `o`'s entries array to `e`.
fn hold_entries(o: *mut Object, e: Value) {
    unsafe {
        (*o).code = e.to_bits() as *const Code;
        crate::heap::barrier(o.cast(), &raw const (*o).code, e);
    }
}

/// Element `i` of entries array `e` (within its length).
fn slot(e: *mut Object, i: usize) -> Value {
    unsafe { *(*e).elems.add(i) }
}

fn len(e: *mut Object) -> usize {
    unsafe { (*e).elems_len as usize }
}

/// A collection's entry width: 2 for the maps, 1 for the sets.
fn stride(o: *mut Object) -> usize {
    if matches!(unsafe { (*o).class }, Class::Map | Class::WeakMap) { 2 } else { 1 }
}

/// The position of `key`'s entry in collection `o`, if it has one.
fn find(o: *mut Object, key: Value) -> Option<usize> {
    let (e, s) = (entries(o), stride(o));
    (0..len(e)).step_by(s).find(|&i| {
        let k = slot(e, i);
        k != Value::EMPTY && same_value_zero(k, key)
    })
}

/// The entries collection `o` has.
fn live(o: *mut Object) -> usize {
    let (e, s) = (entries(o), stride(o));
    (0..len(e)).step_by(s).filter(|&i| slot(e, i) != Value::EMPTY).count()
}

/// Append `entry` (a key, or a key and value) to collection `o`'s
/// entries, compacting them first when more than half are holes.
fn append(vm: &mut Vm, o: *mut Object, entry: &[Value]) -> Result<()> {
    let mut e = entries(o);
    let s = stride(o);
    let n = len(e);
    if n >= 32 && live(o) * s * 2 < n {
        let fresh = vm.new_array();
        for i in (0..n).step_by(s) {
            if slot(e, i) != Value::EMPTY {
                let kv: Vec<Value> = (0..s).map(|j| slot(e, i + j)).collect();
                array_push(vm, fresh, &kv)?;
            }
        }
        hold_entries(o, fresh);
        e = as_object(fresh);
    }
    array_push(vm, object_value(e), entry)?;
    Ok(())
}

/// Remove the entry at `i` of collection `o`: its slots become holes.
fn remove(o: *mut Object, i: usize) {
    let e = entries(o);
    for j in 0..stride(o) {
        unsafe { *(*e).elems.add(i + j) = Value::EMPTY };
    }
}

/// A new, empty collection of class `class`.
fn new_collection(vm: &mut Vm, class: Class, proto: Proto) -> *mut Object {
    let e = vm.new_array();
    let p = object_value(vm.collection_prototypes[proto as usize]);
    let o = vm.new_object_with(class, p, 0);
    hold_entries(o, e);
    o
}

/// Call `f` with each value iterating `v` yields: an array's or typed
/// array's elements, a string's code points, a map's `[key, value]` pairs
/// or a set's keys. (No `Symbol.iterator`: anything else is a TypeError.)
fn iterate(vm: &mut Vm, v: Value, mut f: impl FnMut(&mut Vm, Value) -> Result<()>) -> Result<()> {
    if let Some(u) = string_units(v) {
        let u = u.to_vec();
        let mut i = 0;
        while i < u.len() {
            let c = u[i];
            let item = if (0xD800..0xDC00).contains(&c) && u.get(i + 1).is_some_and(|d| (0xDC00..0xE000).contains(d)) {
                i += 1;
                vm.new_string(&[c, u[i]])
            } else {
                vm.single_char(c)
            };
            i += 1;
            f(vm, item)?;
        }
        return Ok(());
    }
    let class = if v.is_cell() { Some(unsafe { (*as_object(v)).class }) } else { None };
    match class {
        Some(Class::Array | Class::TypedArray) => {
            let a_length = vm.a_length;
            let mut i = 0.0;
            while i < to_number(vm.get(v, a_length)?) {
                let item = vm.get_keyed(v, Value::number(i))?;
                f(vm, item)?;
                i += 1.0;
            }
            Ok(())
        }
        Some(Class::Map | Class::Set) => {
            let o = as_object(v);
            let (e, s) = (entries(o), stride(o));
            let mut i = 0;
            while i < len(e) {
                let k = slot(e, i);
                if k != Value::EMPTY {
                    let item = if s == 2 {
                        let pair = vm.new_array();
                        array_push(vm, pair, &[k, slot(e, i + 1)])?;
                        pair
                    } else {
                        k
                    };
                    f(vm, item)?;
                }
                i += s;
            }
            Ok(())
        }
        _ => Err(Error::type_error(format!("{} is not iterable", crate::vm::display(v)))),
    }
}

/// `new Map(iterable)` and the others: each item the iterable yields (a
/// `[key, value]` pair for the maps) is added through the new object's
/// `set` or `add` method, as JS sees it.
fn collection_ctor(vm: &mut Vm, a: &[Value], class: Class, proto: Proto, name: &str) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error(format!("Constructor {name} requires 'new'")));
    }
    let o = object_value(new_collection(vm, class, proto));
    let iterable = arg(a, 0);
    if iterable.is_undefined() || iterable.is_null() {
        return Ok(o);
    }
    let pairs = matches!(class, Class::Map | Class::WeakMap);
    let a_adder = vm.intern(if pairs { "set" } else { "add" });
    let adder = vm.get(o, a_adder)?;
    if !is_callable(adder) {
        return Err(Error::type_error(format!("'{}' returned for property '{}' of object '#<{name}>' is not a function", crate::vm::display(adder), if pairs { "set" } else { "add" })));
    }
    iterate(vm, iterable, |vm, item| {
        if pairs {
            if !is_object(item) {
                return Err(Error::type_error(format!("Iterator value {} is not an entry object", crate::vm::display(item))));
            }
            let k = vm.get_keyed(item, Value::from_i32(0))?;
            let v = vm.get_keyed(item, Value::from_i32(1))?;
            call_function(vm, adder, o, &[k, v])?;
        } else {
            call_function(vm, adder, o, &[item])?;
        }
        Ok(())
    })?;
    Ok(o)
}

/// `{ value, done }`.
fn iter_result(vm: &mut Vm, value: Value, done: bool) -> Value {
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    let (a_value, a_done) = (vm.intern("value"), vm.intern("done"));
    vm.add_property(r, a_value, value);
    vm.add_property(r, a_done, Value::from_bool(done));
    object_value(r)
}

/// An iterator over collection `o`'s entries from the start.
fn new_iterator(vm: &mut Vm, o: *mut Object, mode: IterMode) -> Value {
    let s = stride(o);
    let proto = vm.collection_prototypes[if s == 2 { Proto::MapIterator } else { Proto::SetIterator } as usize];
    let it = vm.new_object_with(Class::CollectionIterator, object_value(proto), 0);
    hold_entries(it, Value::from_bits(unsafe { (*o).code } as u64));
    unsafe { (*it).elems_cap = mode as u32 | (s as u32) << 2 };
    object_value(it)
}

// ---- Map ----

fn map_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    collection_ctor(vm, a, Class::Map, Proto::Map, "Map")
}

fn map_get(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.get")?;
    Ok(find(o, arg(a, 0)).map_or(Value::UNDEFINED, |i| slot(entries(o), i + 1)))
}

/// `map.set(key, value)`: the map.
fn map_set(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.set")?;
    let (k, v) = (normalize(arg(a, 0)), arg(a, 1));
    match find(o, k) {
        Some(i) => vm.set_keyed(object_value(entries(o)), Value::from_i32(i as i32 + 1), v)?,
        None => append(vm, o, &[k, v])?,
    }
    Ok(this)
}

fn map_has(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.has")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).is_some()))
}

fn map_delete(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.delete")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).inspect(|&i| remove(o, i)).is_some()))
}

/// `map.clear()`: every entry's slots become holes (iterators in progress
/// keep the list, as the spec has it).
fn map_clear(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.clear")?;
    clear(o);
    Ok(Value::UNDEFINED)
}

fn clear(o: *mut Object) {
    let e = entries(o);
    for i in 0..len(e) {
        unsafe { *(*e).elems.add(i) = Value::EMPTY };
    }
}

/// `map.forEach(f, thisArg)`: `f(value, key, map)` for each entry, in
/// insertion order, entries added meanwhile included.
fn map_for_each(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.forEach")?;
    for_each(vm, o, a, this)
}

fn for_each(vm: &mut Vm, o: *mut Object, a: &[Value], this: Value) -> Result<Value> {
    let f = arg(a, 0);
    if !is_callable(f) {
        return Err(Error::type_error(format!("{} is not a function", crate::vm::display(f))));
    }
    let t = arg(a, 1);
    let (e, s) = (entries(o), stride(o));
    let mut i = 0;
    while i < len(e) {
        let k = slot(e, i);
        if k != Value::EMPTY {
            let v = if s == 2 { slot(e, i + 1) } else { k };
            call_function(vm, f, t, &[v, k, this])?;
        }
        i += s;
    }
    Ok(Value::UNDEFINED)
}

fn map_size(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "get Map.prototype.size")?;
    Ok(Value::from_i32(live(o) as i32))
}

fn map_keys(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.keys")?;
    Ok(new_iterator(vm, o, IterMode::Keys))
}

fn map_values(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.values")?;
    Ok(new_iterator(vm, o, IterMode::Values))
}

fn map_entries(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Map, "Map.prototype.entries")?;
    Ok(new_iterator(vm, o, IterMode::Entries))
}

// ---- Set ----

fn set_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    collection_ctor(vm, a, Class::Set, Proto::Set, "Set")
}

/// `set.add(value)`: the set.
fn set_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.add")?;
    let k = normalize(arg(a, 0));
    if find(o, k).is_none() {
        append(vm, o, &[k])?;
    }
    Ok(this)
}

fn set_has(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.has")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).is_some()))
}

fn set_delete(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.delete")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).inspect(|&i| remove(o, i)).is_some()))
}

fn set_clear(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.clear")?;
    clear(o);
    Ok(Value::UNDEFINED)
}

/// `set.forEach(f, thisArg)`: `f(value, value, set)` for each value.
fn set_for_each(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.forEach")?;
    for_each(vm, o, a, this)
}

fn set_size(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "get Set.prototype.size")?;
    Ok(Value::from_i32(live(o) as i32))
}

fn set_values(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.values")?;
    Ok(new_iterator(vm, o, IterMode::Values))
}

fn set_entries(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::Set, "Set.prototype.entries")?;
    Ok(new_iterator(vm, o, IterMode::Entries))
}

// ---- Iterators ----

/// `%MapIteratorPrototype%.next()` and the set one's: the next entry not
/// deleted, as the iterator's mode has it; exhausted for good at the end.
fn iterator_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let it = this_of(this, Class::CollectionIterator, "Iterator.prototype.next")?;
    if unsafe { (*it).code }.is_null() {
        return Ok(iter_result(vm, Value::UNDEFINED, true));
    }
    let e = entries(it);
    let (mode, s) = unsafe { ((*it).elems_cap & 3, ((*it).elems_cap >> 2) as usize) };
    let mut i = unsafe { (*it).elems_len } as usize;
    while i < len(e) {
        let k = slot(e, i);
        i += s;
        if k == Value::EMPTY {
            continue;
        }
        unsafe { (*it).elems_len = i as u32 };
        let v = if s == 2 { slot(e, i - 1) } else { k };
        let value = match mode {
            0 => k,
            1 => v,
            _ => {
                let pair = vm.new_array();
                array_push(vm, pair, &[k, v])?;
                pair
            }
        };
        return Ok(iter_result(vm, value, false));
    }
    unsafe { (*it).code = std::ptr::null() };
    Ok(iter_result(vm, Value::UNDEFINED, true))
}

// ---- WeakMap and WeakSet ----

fn weak_map_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    collection_ctor(vm, a, Class::WeakMap, Proto::WeakMap, "WeakMap")
}

fn weak_map_get(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakMap, "WeakMap.prototype.get")?;
    Ok(find(o, arg(a, 0)).map_or(Value::UNDEFINED, |i| slot(entries(o), i + 1)))
}

/// `weakMap.set(key, value)`: the key must be an object.
fn weak_map_set(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakMap, "WeakMap.prototype.set")?;
    let (k, v) = (arg(a, 0), arg(a, 1));
    if !is_object(k) {
        return Err(Error::type_error("Invalid value used as weak map key"));
    }
    match find(o, k) {
        Some(i) => vm.set_keyed(object_value(entries(o)), Value::from_i32(i as i32 + 1), v)?,
        None => append(vm, o, &[k, v])?,
    }
    Ok(this)
}

fn weak_map_has(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakMap, "WeakMap.prototype.has")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).is_some()))
}

fn weak_map_delete(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakMap, "WeakMap.prototype.delete")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).inspect(|&i| remove(o, i)).is_some()))
}

fn weak_set_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    collection_ctor(vm, a, Class::WeakSet, Proto::WeakSet, "WeakSet")
}

/// `weakSet.add(value)`: the value must be an object.
fn weak_set_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakSet, "WeakSet.prototype.add")?;
    let k = arg(a, 0);
    if !is_object(k) {
        return Err(Error::type_error("Invalid value used in weak set"));
    }
    if find(o, k).is_none() {
        append(vm, o, &[k])?;
    }
    Ok(this)
}

fn weak_set_has(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakSet, "WeakSet.prototype.has")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).is_some()))
}

fn weak_set_delete(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_of(this, Class::WeakSet, "WeakSet.prototype.delete")?;
    Ok(Value::from_bool(find(o, arg(a, 0)).inspect(|&i| remove(o, i)).is_some()))
}

// ---- DataView ----

/// The element types of `DataView`'s `get` and `set` methods, and their
/// sizes (`view_get`'s and `view_set`'s `T` indexes them).
const VIEW_TYPES: [(&str, usize); 8] =
    [("Int8", 1), ("Uint8", 1), ("Int16", 2), ("Uint16", 2), ("Int32", 4), ("Uint32", 4), ("Float32", 4), ("Float64", 8)];

/// `new DataView(buffer, byteOffset, byteLength)`: a view of the buffer's
/// bytes from `byteOffset` (0), `byteLength` of them (the rest).
fn data_view_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor DataView requires 'new'"));
    }
    let buffer = arg(a, 0);
    if !is_class(buffer, Class::ArrayBuffer) {
        return Err(Error::type_error("First argument to DataView constructor must be an ArrayBuffer"));
    }
    if unsafe { (*as_object(buffer)).flags } & crate::object::DETACHED != 0 {
        return Err(Error::type_error("Cannot construct a DataView on a detached ArrayBuffer"));
    }
    let b = as_object(buffer);
    let offset = to_index(vm, arg(a, 1))?;
    let bytes = unsafe { (*b).elems_len } as usize;
    if offset > bytes {
        return Err(Error::range_error(format!("Start offset {offset} is outside the bounds of the buffer")));
    }
    let len = match a.get(2) {
        Some(&v) if !v.is_undefined() => {
            let n = to_index(vm, v)?;
            if offset + n > bytes {
                return Err(Error::range_error(format!("Invalid DataView length {n}")));
            }
            n
        }
        _ => bytes - offset,
    };
    let proto = object_value(vm.collection_prototypes[Proto::DataView as usize]);
    let o = vm.new_object_with(Class::DataView, proto, 0);
    unsafe {
        (*o).elems = (*b).elems.cast::<u8>().add(offset).cast();
        (*o).elems_len = len as u32;
        (*o).code = b.cast();
    }
    Ok(object_value(o))
}

fn this_view(this: Value) -> Result<*mut Object> {
    this_of(this, Class::DataView, "DataView.prototype method")
}

/// Whether view `o`'s buffer was detached (`$262.detachArrayBuffer`).
fn detached(o: *mut Object) -> bool {
    unsafe { (*((*o).code as *mut Object)).flags & crate::object::DETACHED != 0 }
}

/// `this` as a `DataView` whose buffer is attached.
fn this_live_view(this: Value) -> Result<*mut Object> {
    let o = this_view(this)?;
    if detached(o) {
        return Err(Error::type_error("Cannot perform DataView operation on a detached ArrayBuffer"));
    }
    Ok(o)
}

fn view_buffer(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(object_value(unsafe { (*this_view(this)?).code } as *mut Object))
}

fn view_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_i32(unsafe { (*this_live_view(this)?).elems_len } as i32))
}

fn view_byte_offset(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_live_view(this)?;
    unsafe {
        let b = (*o).code as *mut Object;
        Ok(Value::from_i32((*o).elems.cast::<u8>().offset_from((*b).elems.cast::<u8>()) as i32))
    }
}

/// The address of the `size` bytes at `i` in view `o`, if within it.
fn view_bytes(o: *mut Object, i: usize, size: usize) -> Result<*mut u8> {
    if detached(o) {
        return Err(Error::type_error("Cannot perform DataView operation on a detached ArrayBuffer"));
    }
    if i + size > unsafe { (*o).elems_len } as usize {
        return Err(Error::range_error("Offset is outside the bounds of the DataView"));
    }
    Ok(unsafe { (*o).elems.cast::<u8>().add(i) })
}

/// `view.getInt8(byteOffset)`, …, `view.getFloat64(byteOffset,
/// littleEndian)`: the value of type `VIEW_TYPES[T]` there (big-endian
/// unless `littleEndian`).
fn view_get<const T: usize>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_view(this)?;
    let i = to_index(vm, arg(a, 0))?;
    let little = to_boolean(arg(a, 1));
    let size = VIEW_TYPES[T].1;
    let p = view_bytes(o, i, size)?;
    let mut b = [0u8; 8];
    unsafe { std::ptr::copy_nonoverlapping(p, b.as_mut_ptr(), size) };
    if !little {
        b[..size].reverse();
    }
    Ok(match T {
        0 => Value::from_i32(b[0] as i8 as i32),
        1 => Value::from_i32(b[0] as i32),
        2 => Value::from_i32(i16::from_le_bytes([b[0], b[1]]) as i32),
        3 => Value::from_i32(u16::from_le_bytes([b[0], b[1]]) as i32),
        4 => Value::from_i32(i32::from_le_bytes([b[0], b[1], b[2], b[3]])),
        5 => Value::number(u32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
        6 => Value::number(f32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f64),
        _ => Value::number(f64::from_le_bytes(b)),
    })
}

/// `view.setInt8(byteOffset, value)`, …, `view.setFloat64(byteOffset,
/// value, littleEndian)`: the offset is converted, then the value, then
/// the offset checked (as the spec orders it).
fn view_set<const T: usize>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_view(this)?;
    let i = to_index(vm, arg(a, 0))?;
    let d = to_number(arg(a, 1));
    if vm.conv_thrown {
        return Err(vm.take_conv_error());
    }
    let little = to_boolean(arg(a, 2));
    let size = VIEW_TYPES[T].1;
    let p = view_bytes(o, i, size)?;
    let mut b = [0u8; 8];
    match T {
        0 | 1 => b[0] = to_int32(d) as u8,
        2 | 3 => b[..2].copy_from_slice(&(to_int32(d) as u16).to_le_bytes()),
        4 | 5 => b[..4].copy_from_slice(&(to_int32(d) as u32).to_le_bytes()),
        6 => b[..4].copy_from_slice(&(d as f32).to_le_bytes()),
        _ => b = d.to_le_bytes(),
    }
    if !little {
        b[..size].reverse();
    }
    unsafe { std::ptr::copy_nonoverlapping(b.as_ptr(), p, size) };
    Ok(Value::UNDEFINED)
}
