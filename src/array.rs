//! `Array.prototype`'s methods, generic as the spec has them (over any
//! object with a `length`, through the generic property operations of
//! `props`), with fast paths over a plain array's elements. Installed by
//! [`install`] (`push` is `vm::array_push`, which compiled code calls).

use crate::interp::{call_function, strict_equals, to_boolean, to_number, to_string_units};
use crate::object::{Class, EXTERNAL, Object, PROTOTYPE, as_object, object_value};
use crate::props::{is_callable, is_object, same_value};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, array_push, string_units};

pub(crate) fn install(vm: &mut Vm) {
    let ap = vm.array_prototype;
    vm.define_natives(
        ap,
        &[
            ("toString", array_to_string as NativeFn, 0),
            ("toLocaleString", array_to_locale_string, 0),
            ("join", array_join, 1),
            ("concat", array_concat, 1),
            ("reverse", array_reverse, 0),
            ("shift", array_shift, 0),
            ("unshift", array_unshift, 1),
            ("slice", array_slice, 2),
            ("splice", array_splice, 2),
            ("sort", array_sort, 1),
            ("lastIndexOf", array_last_index_of, 1),
            ("every", array_every, 1),
            ("some", array_some, 1),
            ("forEach", array_for_each, 1),
            ("map", array_map, 1),
            ("filter", array_filter, 1),
            ("reduce", array_reduce, 1),
            ("reduceRight", array_reduce_right, 1),
            ("fill", array_fill, 1),
            ("find", array_find, 1),
            ("findIndex", array_find_index, 1),
            ("includes", array_includes, 1),
            ("copyWithin", array_copy_within, 2),
            ("flat", array_flat, 0),
            ("flatMap", array_flat_map, 1),
            ("at", array_at, 1),
            ("findLast", array_find_last, 1),
            ("findLastIndex", array_find_last_index, 1),
            ("toReversed", array_to_reversed, 0),
            ("toSorted", array_to_sorted, 1),
            ("toSpliced", array_to_spliced, 2),
            ("with", array_with, 2),
        ],
    );
    let a_array = vm.intern("Array");
    let array = as_object(vm.get(object_value(vm.global), a_array).unwrap());
    vm.define_natives(
        array,
        &[
            ("isArray", array_is_array as NativeFn, 1),
            ("of", array_of, 0),
            ("from", array_from, 1),
            ("fromAsync", array_from_async, 1),
        ],
    );
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// ToIntegerOrInfinity.
fn to_integer(v: Value) -> f64 {
    let d = to_number(v);
    if d.is_nan() { 0.0 } else { d.trunc() }
}

/// A relative index argument (`slice`'s): `default` if undefined, else
/// counted from `len` if negative, clamped to `0..=len`.
fn relative(v: Value, len: f64, default: f64) -> f64 {
    if v.is_undefined() {
        return default;
    }
    let n = to_integer(v);
    if n < 0.0 { (len + n).max(0.0) } else { n.min(len) }
}

/// Whether `o` is a plain array whose elements the fast paths may read
/// directly: no attributes on them, and nothing indexed on its prototype
/// chain (holes are then absent), `Array.prototype` then
/// `Object.prototype`.
fn dense(vm: &Vm, o: *mut Object) -> bool {
    unsafe {
        (*o).class == Class::Array
            && (*o).flags & !(EXTERNAL | PROTOTYPE) == 0
            && (*(*o).shape).proto == object_value(vm.array_prototype)
            && (*(*vm.array_prototype).shape).proto == object_value(vm.object_prototype)
            && (*(*vm.object_prototype).shape).proto.is_null()
            && (*vm.array_prototype).elems_len == 0
            && (*vm.object_prototype).elems_len == 0
            && (*vm.array_prototype).flags & crate::object::SPARSE == 0
            && (*vm.object_prototype).flags & crate::object::SPARSE == 0
    }
}

/// A key for index `i` (an `f64`, as lengths go to 2^53).
fn key(i: f64) -> Key {
    if i < u32::MAX as f64 {
        Key::Index(i as u32)
    } else {
        let vm = unsafe { &mut *crate::interp::current_vm() };
        Key::Name(vm.intern(&format!("{i}")))
    }
}

/// `o`'s element `i`, `has` saying whether it is there (`HasProperty`).
fn element(vm: &mut Vm, o: Value, i: f64) -> Result<(bool, Value)> {
    let obj = as_object(o);
    if dense(vm, obj) && i < u32::MAX as f64 {
        unsafe {
            let i = i as u32;
            if i < (*obj).elems_len {
                let v = *(*obj).elems.add(i as usize);
                return Ok(if v == Value::EMPTY { (false, Value::UNDEFINED) } else { (true, v) });
            }
            return Ok((false, Value::UNDEFINED));
        }
    }
    let k = key(i);
    if !vm.has_key(o, k)? {
        return Ok((false, Value::UNDEFINED));
    }
    Ok((true, vm.get_key(o, k)?))
}

fn get(vm: &mut Vm, o: Value, i: f64) -> Result<Value> {
    vm.get_key(o, key(i))
}

/// `Set(o, i, v, true)`.
fn put(vm: &mut Vm, o: Value, i: f64, v: Value) -> Result<()> {
    vm.put_key(o, key(i), v)
}

fn delete(vm: &mut Vm, o: Value, i: f64) -> Result<()> {
    vm.delete_key(o, key(i))
}

/// `o.length = n` (`Set` with throw).
fn set_length(vm: &mut Vm, o: Value, n: f64) -> Result<()> {
    let a_length = vm.a_length;
    vm.put_key(o, Key::Name(a_length), Value::number(n))
}

/// `this` as an object, and its length.
fn this_object(vm: &mut Vm, this: Value) -> Result<(Value, f64)> {
    let o = object_value(vm.to_object(this)?);
    let len = vm.length_of(o)?;
    Ok((o, len))
}

/// The callback argument, checked callable.
fn callback(a: &[Value], what: &str) -> Result<Value> {
    let f = arg(a, 0);
    if !is_callable(f) {
        return Err(Error::type_error(format!(
            "{} is not a function (Array.prototype.{what})",
            crate::vm::display(f)
        )));
    }
    Ok(f)
}

/// A new array of length `n` (ArraySpeciesCreate without species).
fn new_array_len(vm: &mut Vm, n: f64) -> Result<Value> {
    if n > u32::MAX as f64 {
        return Err(Error::range_error("Invalid array length"));
    }
    let a = vm.new_array();
    if n > 0.0 {
        let a_length = vm.a_length;
        vm.set(a, a_length, Value::number(n))?;
    }
    Ok(a)
}

/// ArraySpeciesCreate(original, n): a new array, or (an array whose
/// `constructor[@@species]` says so) that constructor's object.
fn species_create(vm: &mut Vm, original: Value, n: f64) -> Result<Value> {
    if !crate::proxy::is_array(vm, original)? {
        return new_array_len(vm, n);
    }
    let a_constructor = vm.a_constructor;
    let mut c = vm.get(original, a_constructor)?;
    // (Another realm's `Array`: an array of this one's.)
    if !vm.realms.is_empty() && crate::intrinsics::is_constructor(c) {
        let r = crate::realm::function_realm(vm, c)?;
        if r != vm.realm && crate::realm::intrinsic(vm, r, |vm| object_value(vm.array_fn)) == c {
            c = Value::UNDEFINED;
        }
    }
    if crate::props::is_object(c) {
        let a_species = vm.intern(&format!("{}Symbol.species", crate::symbol::SYMBOL_MARK));
        c = vm.get(c, a_species)?;
        if c.is_null() {
            c = Value::UNDEFINED;
        }
    }
    if c.is_undefined() || (c.is_cell() && as_object(c) == vm.array_fn) {
        return new_array_len(vm, n);
    }
    if !crate::intrinsics::is_constructor(c) {
        return Err(Error::type_error("object.constructor[Symbol.species] is not a constructor"));
    }
    crate::intrinsics::construct(vm, c, &[Value::number(n)])
}

/// `Set(o, "length", n, true)`: a failure throws.
fn set_length_or_throw(vm: &mut Vm, o: Value, n: f64) -> Result<()> {
    let a_length = vm.a_length;
    vm.put_key(o, Key::Name(a_length), Value::number(n))
}

/// Whether `concat` spreads `v`: `v[@@isConcatSpreadable]`, else IsArray.
fn spreadable(vm: &mut Vm, v: Value) -> Result<bool> {
    if !crate::props::is_object(v) {
        return Ok(false);
    }
    let a = vm.intern(&format!("{}Symbol.isConcatSpreadable", crate::symbol::SYMBOL_MARK));
    let s = vm.get(v, a)?;
    if !s.is_undefined() {
        return Ok(to_boolean(s));
    }
    crate::proxy::is_array(vm, v)
}

/// FlattenIntoArray: `source`'s elements into `target` from `start`, to
/// `depth` (mapped first by `f` with `that`, if given): the next index.
fn flatten(vm: &mut Vm, target: Value, source: Value, len: f64, start: f64, depth: f64, f: Option<(Value, Value)>) -> Result<f64> {
    // (Nested deeply, or in a cycle, as deep as `depth`: a RangeError.)
    crate::interp::check_native_stack(vm)?;
    let mut at = start;
    let mut i = 0.0;
    while i < len {
        let (has, mut v) = element(vm, source, i)?;
        if has {
            if let Some((f, that)) = f {
                v = call_function(vm, f, that, &[v, Value::number(i), source])?;
            }
            if depth > 0.0 && crate::proxy::is_array(vm, v)? {
                let n = vm.length_of(v)?;
                at = flatten(vm, target, v, n, at, depth - 1.0, None)?;
            } else {
                create(vm, target, at, v)?;
                at += 1.0;
            }
        }
        i += 1.0;
    }
    Ok(at)
}

fn array_flat(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let depth = if arg(a, 0).is_undefined() { 1.0 } else { to_integer(arg(a, 0)).max(0.0) };
    let out = species_create(vm, o, 0.0)?;
    flatten(vm, out, o, len, 0.0, depth, None)?;
    Ok(out)
}

fn array_flat_map(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let f = callback(a, "flatMap")?;
    let out = species_create(vm, o, 0.0)?;
    flatten(vm, out, o, len, 0.0, 1.0, Some((f, arg(a, 1))))?;
    Ok(out)
}

/// A new array of `values`.
pub(crate) fn new_array_of(vm: &mut Vm, values: &[Value]) -> Result<Value> {
    let a = vm.new_array();
    array_push(vm, a, values)?;
    Ok(a)
}

/// `CreateDataPropertyOrThrow(o, i, v)`.
fn create(vm: &mut Vm, o: Value, i: f64, v: Value) -> Result<()> {
    vm.create_data_property(o, key(i), v)
}

fn array_is_array(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    if vm.proxies_exist {
        return crate::proxy::is_array(vm, v).map(Value::from_bool);
    }
    Ok(Value::from_bool(v.is_cell() && unsafe { (*as_object(v)).class } == Class::Array))
}

/// `Array.prototype.join(sep)`: elements' strings (holes, `undefined` and
/// `null` empty), separated by `sep` (a comma by default).
/// Whether `o` is being joined already (an array holding itself): joins
/// root their arrays as handles marked by a hole before them, apart from
/// other handles (`apply`'s argument lists, say).
fn joining(vm: &Vm, o: Value) -> bool {
    vm.handles.windows(2).any(|w| w[0] == Value::HOLE && w[1] == o)
}

fn array_join(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let sep: Vec<u16> = match a.first() {
        Some(v) if !v.is_undefined() => to_string_units(*v),
        _ => vec![b',' as u16],
    };
    // A cycle (an array holding itself) joins as empty, as the engines do.
    if joining(vm, o) {
        return Ok(vm.new_string(&[]));
    }
    vm.handle_scope(|vm| {
        vm.handles.extend([Value::HOLE, o]);
        let mut out: Vec<u16> = Vec::new();
        let mut i = 0.0;
        while i < len {
            if i > 0.0 {
                out.extend_from_slice(&sep);
            }
            let v = get(vm, o, i)?;
            if !v.is_undefined() && !v.is_null() {
                out.extend(to_string_units(v));
                // (Its exception stops the join: no next element read.)
                if vm.conv_failed() {
                    return Err(vm.take_conv_error());
                }
            }
            i += 1.0;
        }
        Ok(vm.new_string(&out))
    })
}

/// `Array.prototype.toString()`: `join()`, or `Object.prototype.toString`
/// where there is no callable `join`.
fn array_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = object_value(vm.to_object(this)?);
    let a_join = vm.intern("join");
    let f = vm.get(o, a_join)?;
    if is_callable(f) {
        return call_function(vm, f, o, &[]);
    }
    let a = vm.intern("toString");
    let op = object_value(vm.object_prototype);
    let f = vm.get(op, a)?;
    call_function(vm, f, o, &[])
}

/// `Array.prototype.toLocaleString()`: each element's `toLocaleString()`,
/// comma-separated.
pub(crate) fn array_to_locale_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    if joining(vm, o) {
        return Ok(vm.new_string(&[]));
    }
    vm.handle_scope(|vm| {
        vm.handles.extend([Value::HOLE, o]);
        let a_tls = vm.intern("toLocaleString");
        let mut out: Vec<u16> = Vec::new();
        let mut i = 0.0;
        while i < len {
            if i > 0.0 {
                out.push(b',' as u16);
            }
            let v = get(vm, o, i)?;
            if !v.is_undefined() && !v.is_null() {
                let f = vm.get(v, a_tls)?;
                if !is_callable(f) {
                    return Err(Error::type_error("toLocaleString is not a function"));
                }
                let s = call_function(vm, f, v, &[])?;
                out.extend(to_string_units(s));
                if vm.conv_failed() {
                    return Err(vm.take_conv_error());
                }
            }
            i += 1.0;
        }
        Ok(vm.new_string(&out))
    })
}

/// `Array.prototype.concat(…items)`: arrays spread, other values appended.
fn array_concat(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = object_value(vm.to_object(this)?);
    let out = species_create(vm, o, 0.0)?;
    let mut n = 0.0;
    for &item in std::iter::once(&o).chain(a) {
        let spread = spreadable(vm, item)?;
        if !spread {
            if n >= 9007199254740991.0 {
                return Err(Error::type_error("Array length exceeds the maximum"));
            }
            create(vm, out, n, item)?;
            n += 1.0;
            continue;
        }
        let len = vm.length_of(item)?;
        if n + len > 9007199254740991.0 {
            return Err(Error::type_error("Array length exceeds the maximum"));
        }
        let obj = as_object(item);
        if unsafe { (*obj).class } == Class::Array && dense(vm, obj) && dense(vm, as_object(out)) {
            unsafe {
                let m = (*obj).elems_len as usize;
                let oo = as_object(out);
                let at = (*oo).elems_len as usize;
                vm.reserve_elems(oo, at + m);
                for i in 0..m {
                    let v = *(*obj).elems.add(i);
                    if v == Value::EMPTY {
                        *(*oo).elems.add(at + i) = Value::EMPTY;
                    } else {
                        vm.store_element(oo, at + i, v);
                    }
                }
                (*oo).elems_len = (at + m) as u32;
            }
            n += len;
            continue;
        }
        let mut i = 0.0;
        while i < len {
            let (has, v) = element(vm, item, i)?;
            if has {
                create(vm, out, n, v)?;
            }
            i += 1.0;
            n += 1.0;
        }
    }
    set_length(vm, out, n)?;
    Ok(out)
}

/// `Array.prototype.pop()`.
pub(crate) fn array_pop(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if this.is_cell() && dense(vm, as_object(this)) {
        let o = as_object(this);
        unsafe {
            let len = (*o).elems_len as usize;
            if len == 0 {
                return Ok(Value::UNDEFINED);
            }
            let v = *(*o).elems.add(len - 1);
            *(*o).elems.add(len - 1) = Value::EMPTY;
            (*o).elems_len = (len - 1) as u32;
            return Ok(if v == Value::EMPTY { Value::UNDEFINED } else { v });
        }
    }
    let (o, len) = this_object(vm, this)?;
    if len == 0.0 {
        set_length(vm, o, 0.0)?;
        return Ok(Value::UNDEFINED);
    }
    let v = get(vm, o, len - 1.0)?;
    delete(vm, o, len - 1.0)?;
    set_length(vm, o, len - 1.0)?;
    Ok(v)
}

/// `Array.prototype.reverse()`, in place.
pub(crate) fn array_reverse(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    if dense(vm, as_object(o)) {
        let obj = as_object(o);
        unsafe {
            let n = (*obj).elems_len as usize;
            let e = std::slice::from_raw_parts_mut((*obj).elems, n);
            e.reverse();
            // (Every slot may now hold what was elsewhere: barriered.)
            for i in 0..n {
                let v = *(*obj).elems.add(i);
                crate::heap::barrier((*obj).elems.cast::<u8>().sub(8), (*obj).elems.add(i), v);
            }
        }
        return Ok(o);
    }
    let mut lower = 0.0;
    while lower < (len / 2.0).floor() {
        let upper = len - lower - 1.0;
        let (lower_has, lower_v) = element(vm, o, lower)?;
        let (upper_has, upper_v) = element(vm, o, upper)?;
        match (lower_has, upper_has) {
            (true, true) => {
                put(vm, o, lower, upper_v)?;
                put(vm, o, upper, lower_v)?;
            }
            (false, true) => {
                put(vm, o, lower, upper_v)?;
                delete(vm, o, upper)?;
            }
            (true, false) => {
                delete(vm, o, lower)?;
                put(vm, o, upper, lower_v)?;
            }
            (false, false) => {}
        }
        lower += 1.0;
    }
    Ok(o)
}

/// `Array.prototype.shift()`: the first element, the rest moved down.
fn array_shift(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    if len == 0.0 {
        set_length(vm, o, 0.0)?;
        return Ok(Value::UNDEFINED);
    }
    if dense(vm, as_object(o)) {
        let obj = as_object(o);
        unsafe {
            let n = (*obj).elems_len as usize;
            let first = *(*obj).elems;
            std::ptr::copy((*obj).elems.add(1), (*obj).elems, n - 1);
            *(*obj).elems.add(n - 1) = Value::EMPTY;
            (*obj).elems_len = (n - 1) as u32;
            for i in 0..n - 1 {
                let v = *(*obj).elems.add(i);
                crate::heap::barrier((*obj).elems.cast::<u8>().sub(8), (*obj).elems.add(i), v);
            }
            return Ok(if first == Value::EMPTY { Value::UNDEFINED } else { first });
        }
    }
    let first = get(vm, o, 0.0)?;
    let mut k = 1.0;
    while k < len {
        let (has, v) = element(vm, o, k)?;
        if has {
            put(vm, o, k - 1.0, v)?;
        } else {
            delete(vm, o, k - 1.0)?;
        }
        k += 1.0;
    }
    delete(vm, o, len - 1.0)?;
    set_length(vm, o, len - 1.0)?;
    Ok(first)
}

/// `Array.prototype.unshift(…items)`: the items prepended; the length.
fn array_unshift(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let n = a.len() as f64;
    if n > 0.0 {
        if len + n > 9007199254740991.0 {
            return Err(Error::type_error("Array length exceeds the maximum"));
        }
        if dense(vm, as_object(o)) {
            let obj = as_object(o);
            unsafe {
                let m = (*obj).elems_len as usize;
                vm.reserve_elems(obj, m + a.len());
                std::ptr::copy((*obj).elems, (*obj).elems.add(a.len()), m);
                for (i, &v) in a.iter().enumerate() {
                    *(*obj).elems.add(i) = Value::EMPTY;
                    vm.store_element(obj, i, v);
                }
                (*obj).elems_len = (m + a.len()) as u32;
                // (Moved elements keep the shape's elements type.)
                for i in a.len()..m + a.len() {
                    let v = *(*obj).elems.add(i);
                    crate::heap::barrier((*obj).elems.cast::<u8>().sub(8), (*obj).elems.add(i), v);
                }
            }
            return Ok(Value::number(len + n));
        }
        let mut k = len;
        while k > 0.0 {
            let (has, v) = element(vm, o, k - 1.0)?;
            if has {
                put(vm, o, k + n - 1.0, v)?;
            } else {
                delete(vm, o, k + n - 1.0)?;
            }
            k -= 1.0;
        }
        for (j, &v) in a.iter().enumerate() {
            put(vm, o, j as f64, v)?;
        }
    }
    set_length(vm, o, len + n)?;
    Ok(Value::number(len + n))
}

/// `Array.prototype.slice(start, end)`.
fn array_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let k = relative(arg(a, 0), len, 0.0);
    let end = relative(arg(a, 1), len, len);
    let count = (end - k).max(0.0);
    let out = species_create(vm, o, count)?;
    // (The arguments' conversions and the species lookup may have run
    // code that changed the length: the fast path only over the one
    // measured.)
    if dense(vm, as_object(o)) && dense(vm, as_object(out)) && unsafe { (*as_object(o)).elems_len } as f64 == len {
        let obj = as_object(o);
        let oo = as_object(out);
        unsafe {
            let (k, count) = (k as usize, count as usize);
            vm.reserve_elems(oo, count);
            // The result takes the source's elements' type at once, which
            // all the values fit: copied, not checked and stored one by one
            // (PdfJS's `decrypt`, `decryptedString.slice(n)`, spent more there
            // than in its loop).
            let want = (*(*oo).shape).elems.join((*(*obj).shape).elems);
            if want != (*(*oo).shape).elems {
                (*oo).shape = vm.elements_transition((*oo).shape, want);
            }
            // (Numbers alone, no cells: nothing for the barrier to note.)
            if matches!(want, crate::object::FieldType::Int | crate::object::FieldType::Num) {
                std::ptr::copy_nonoverlapping((*obj).elems.add(k), (*oo).elems, count);
            } else {
                vm.store_elements(oo, 0, (*obj).elems.add(k), count);
            }
            (*oo).elems_len = count as u32;
        }
        return Ok(out);
    }
    let mut n = 0.0;
    let mut i = k;
    while i < end {
        let (has, v) = element(vm, o, i)?;
        if has {
            create(vm, out, n, v)?;
        }
        i += 1.0;
        n += 1.0;
    }
    set_length(vm, out, n)?;
    Ok(out)
}

/// `Array.prototype.splice(start, deleteCount, …items)`: the removed
/// elements, as an array.
fn array_splice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let start = relative(arg(a, 0), len, 0.0);
    let (del, items): (f64, &[Value]) = match a.len() {
        0 => (0.0, &[]),
        1 => (len - start, &[]),
        _ => ((to_integer(a[1])).clamp(0.0, len - start), &a[2..]),
    };
    let n = items.len() as f64;
    if len + n - del > 9007199254740991.0 {
        return Err(Error::type_error("Array length exceeds the maximum"));
    }
    // (ArraySpeciesCreate: a subclass's `splice` gives one of its own.)
    let removed = species_create(vm, o, del)?;
    // (As in `slice`: `start` and `deleteCount`'s conversions may have
    // changed the length. The fast path writes `removed`'s elements: an
    // array's of its own.)
    if dense(vm, as_object(o))
        && unsafe { (*as_object(o)).elems_len } as f64 == len
        && is_object(removed)
        && dense(vm, as_object(removed))
    {
        let obj = as_object(o);
        let ro = as_object(removed);
        unsafe {
            let (start, del) = (start as usize, del as usize);
            let m = (*obj).elems_len as usize;
            vm.reserve_elems(ro, del);
            for i in 0..del {
                let v = *(*obj).elems.add(start + i);
                if v == Value::EMPTY {
                    *(*ro).elems.add(i) = Value::EMPTY;
                } else {
                    vm.store_element(ro, i, v);
                }
            }
            (*ro).elems_len = del as u32;
            let new_len = m - del + items.len();
            vm.reserve_elems(obj, new_len);
            std::ptr::copy((*obj).elems.add(start + del), (*obj).elems.add(start + items.len()), m - start - del);
            for (i, &v) in items.iter().enumerate() {
                *(*obj).elems.add(start + i) = Value::EMPTY;
                vm.store_element(obj, start + i, v);
            }
            for i in new_len..m {
                *(*obj).elems.add(i) = Value::EMPTY;
            }
            (*obj).elems_len = new_len as u32;
            for i in start + items.len()..new_len {
                let v = *(*obj).elems.add(i);
                crate::heap::barrier((*obj).elems.cast::<u8>().sub(8), (*obj).elems.add(i), v);
            }
        }
        return Ok(removed);
    }
    let mut k = 0.0;
    while k < del {
        let (has, v) = element(vm, o, start + k)?;
        if has {
            create(vm, removed, k, v)?;
        }
        k += 1.0;
    }
    set_length(vm, removed, del)?;
    if n < del {
        let mut k = start;
        while k < len - del {
            let (has, v) = element(vm, o, k + del)?;
            if has {
                put(vm, o, k + n, v)?;
            } else {
                delete(vm, o, k + n)?;
            }
            k += 1.0;
        }
        let mut k = len;
        while k > len - del + n {
            delete(vm, o, k - 1.0)?;
            k -= 1.0;
        }
    } else if n > del {
        let mut k = len - del;
        while k > start {
            let (has, v) = element(vm, o, k + del - 1.0)?;
            if has {
                put(vm, o, k + n - 1.0, v)?;
            } else {
                delete(vm, o, k + n - 1.0)?;
            }
            k -= 1.0;
        }
    }
    for (i, &v) in items.iter().enumerate() {
        put(vm, o, start + i as f64, v)?;
    }
    set_length(vm, o, len - del + n)?;
    Ok(removed)
}

/// `Array.prototype.sort(compare)`: stable; `undefined`s last, holes
/// after them.
fn array_sort(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let cmp = arg(a, 0);
    if !cmp.is_undefined() && !is_callable(cmp) {
        return Err(Error::type_error("The comparison function must be either a function or undefined"));
    }
    let (o, len) = this_object(vm, this)?;
    vm.handle_scope(|vm| {
        let mut values = Vec::new();
        let mut present = 0.0;
        let mut i = 0.0;
        while i < len {
            let (has, v) = element(vm, o, i)?;
            if has {
                vm.handles.push(v);
                values.push(v);
                present += 1.0;
            }
            i += 1.0;
        }
        let (mut defined, undef): (Vec<Value>, Vec<Value>) = values.into_iter().partition(|x| !x.is_undefined());
        let mut err = None;
        let mut after = |vm: &mut Vm, x: Value, y: Value| -> bool {
            if err.is_some() {
                return false;
            }
            // (A conversion's exception ends the sort as a comparator's.)
            let r = if cmp.is_undefined() {
                to_string_units(x) > to_string_units(y)
            } else {
                match call_function(vm, cmp, Value::UNDEFINED, &[x, y]) {
                    Ok(r) => to_number(r) > 0.0,
                    Err(e) => {
                        err = Some(e);
                        return false;
                    }
                }
            };
            if vm.conv_failed() {
                err = Some(vm.take_conv_error());
            }
            r
        };
        merge_sort(vm, &mut defined, &mut after);
        if let Some(e) = err {
            return Err(e);
        }
        defined.extend(undef);
        for (i, &v) in defined.iter().enumerate() {
            put(vm, o, i as f64, v)?;
        }
        let mut i = present;
        while i < len {
            delete(vm, o, i)?;
            i += 1.0;
        }
        Ok(o)
    })
}

/// Sort `v` stably, `after(x, y)` meaning `x` goes after `y`.
fn merge_sort(vm: &mut Vm, v: &mut [Value], after: &mut dyn FnMut(&mut Vm, Value, Value) -> bool) {
    let n = v.len();
    if n <= 10 {
        for i in 1..n {
            let mut j = i;
            while j > 0 && after(vm, v[j - 1], v[j]) {
                v.swap(j - 1, j);
                j -= 1;
            }
        }
        return;
    }
    let mid = n / 2;
    merge_sort(vm, &mut v[..mid], after);
    merge_sort(vm, &mut v[mid..], after);
    let left = v[..mid].to_vec();
    let right = v[mid..].to_vec();
    let (mut i, mut j, mut k) = (0, 0, 0);
    while i < left.len() && j < right.len() {
        if after(vm, left[i], right[j]) {
            v[k] = right[j];
            j += 1;
        } else {
            v[k] = left[i];
            i += 1;
        }
        k += 1;
    }
    while i < left.len() {
        v[k] = left[i];
        i += 1;
        k += 1;
    }
    while j < right.len() {
        v[k] = right[j];
        j += 1;
        k += 1;
    }
}

/// `Array.prototype.indexOf(x, from)`.
pub(crate) fn array_index_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let x = arg(a, 0);
    if len == 0.0 {
        return Ok(Value::from_i32(-1));
    }
    let n = if a.len() > 1 { to_integer(a[1]) } else { 0.0 };
    if n >= len {
        return Ok(Value::from_i32(-1));
    }
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    if dense(vm, as_object(o)) {
        let obj = as_object(o);
        unsafe {
            // (Not past the length it had: `fromIndex`'s conversion may have
            // pushed more.)
            let m = ((*obj).elems_len as usize).min(len as usize);
            for i in k as usize..m {
                let e = *(*obj).elems.add(i);
                if e != Value::EMPTY && strict_equals(e, x) {
                    return Ok(Value::from_i32(i as i32));
                }
            }
        }
        return Ok(Value::from_i32(-1));
    }
    while k < len {
        let (has, v) = element(vm, o, k)?;
        if has && strict_equals(v, x) {
            return Ok(Value::number(k));
        }
        k += 1.0;
    }
    Ok(Value::from_i32(-1))
}

/// `Array.prototype.lastIndexOf(x, from)`.
pub(crate) fn array_last_index_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let x = arg(a, 0);
    if len == 0.0 {
        return Ok(Value::from_i32(-1));
    }
    let n = if a.len() > 1 { to_integer(a[1]) } else { len - 1.0 };
    let mut k = if n >= 0.0 { n.min(len - 1.0) } else { len + n };
    while k >= 0.0 {
        let (has, v) = element(vm, o, k)?;
        if has && strict_equals(v, x) {
            // (`-0` as `+0`.)
            return Ok(Value::number(k + 0.0));
        }
        k -= 1.0;
    }
    Ok(Value::from_i32(-1))
}

/// `Array.prototype.includes(x, from)`: SameValueZero, holes as
/// `undefined`.
pub(crate) fn array_includes(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let x = arg(a, 0);
    if len == 0.0 {
        return Ok(Value::FALSE);
    }
    let n = if a.len() > 1 { to_integer(a[1]) } else { 0.0 };
    let mut k = if n >= 0.0 { n } else { (len + n).max(0.0) };
    while k < len {
        let v = get(vm, o, k)?;
        let same = same_value(v, x) || v.as_number().is_some_and(|d| d == 0.0 && x.as_number() == Some(0.0));
        if same {
            return Ok(Value::TRUE);
        }
        k += 1.0;
    }
    Ok(Value::FALSE)
}

/// The iteration `Array.prototype.every` and the like share: `on(i, v, r)`
/// with the callback's result `r` for each present element, until it
/// says to stop.
fn each(
    vm: &mut Vm,
    this: Value,
    a: &[Value],
    what: &str,
    mut on: impl FnMut(&mut Vm, f64, Value, Value) -> Result<bool>,
) -> Result<()> {
    let (o, len) = this_object(vm, this)?;
    let f = callback(a, what)?;
    let that = arg(a, 1);
    let mut i = 0.0;
    while i < len {
        let (has, v) = element(vm, o, i)?;
        if has {
            let r = call_function(vm, f, that, &[v, Value::number(i), o])?;
            if !on(vm, i, v, r)? {
                break;
            }
        }
        i += 1.0;
    }
    Ok(())
}

pub(crate) fn array_every(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let mut all = true;
    each(vm, this, a, "every", |_, _, _, r| {
        all = to_boolean(r);
        Ok(all)
    })?;
    Ok(Value::from_bool(all))
}

pub(crate) fn array_some(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let mut any = false;
    each(vm, this, a, "some", |_, _, _, r| {
        any = to_boolean(r);
        Ok(!any)
    })?;
    Ok(Value::from_bool(any))
}

pub(crate) fn array_for_each(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    each(vm, this, a, "forEach", |_, _, _, _| Ok(true))?;
    Ok(Value::UNDEFINED)
}

fn array_map(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    callback(a, "map")?;
    let out = species_create(vm, o, len)?;
    each(vm, o, a, "map", |vm, i, _, r| {
        create(vm, out, i, r)?;
        Ok(true)
    })?;
    Ok(out)
}

fn array_filter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, _) = this_object(vm, this)?;
    callback(a, "filter")?;
    let out = species_create(vm, o, 0.0)?;
    let mut n = 0.0;
    each(vm, this, a, "filter", |vm, _, v, r| {
        if to_boolean(r) {
            create(vm, out, n, v)?;
            n += 1.0;
        }
        Ok(true)
    })?;
    Ok(out)
}

pub(crate) fn array_find(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let f = callback(a, "find")?;
    let that = arg(a, 1);
    let mut i = 0.0;
    while i < len {
        let v = get(vm, o, i)?;
        let r = call_function(vm, f, that, &[v, Value::number(i), o])?;
        if to_boolean(r) {
            return Ok(v);
        }
        i += 1.0;
    }
    Ok(Value::UNDEFINED)
}

pub(crate) fn array_find_index(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let f = callback(a, "findIndex")?;
    let that = arg(a, 1);
    let mut i = 0.0;
    while i < len {
        let v = get(vm, o, i)?;
        let r = call_function(vm, f, that, &[v, Value::number(i), o])?;
        if to_boolean(r) {
            return Ok(Value::number(i));
        }
        i += 1.0;
    }
    Ok(Value::from_i32(-1))
}

/// `reduce` (`right`: `reduceRight`).
fn reduce(vm: &mut Vm, this: Value, a: &[Value], right: bool) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let f = callback(a, if right { "reduceRight" } else { "reduce" })?;
    let mut k = if right { len - 1.0 } else { 0.0 };
    let step = if right { -1.0 } else { 1.0 };
    let in_range = |k: f64| if right { k >= 0.0 } else { k < len };
    let mut acc = if a.len() > 1 {
        a[1]
    } else {
        let mut found = None;
        while in_range(k) {
            let (has, v) = element(vm, o, k)?;
            k += step;
            if has {
                found = Some(v);
                break;
            }
        }
        found.ok_or_else(|| Error::type_error("Reduce of empty array with no initial value"))?
    };
    while in_range(k) {
        let (has, v) = element(vm, o, k)?;
        if has {
            acc = call_function(vm, f, Value::UNDEFINED, &[acc, v, Value::number(k), o])?;
        }
        k += step;
    }
    Ok(acc)
}

pub(crate) fn array_reduce(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    reduce(vm, this, a, false)
}

pub(crate) fn array_reduce_right(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    reduce(vm, this, a, true)
}

/// `Array.prototype.fill(v, start, end)`.
fn array_fill(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let mut k = relative(arg(a, 1), len, 0.0);
    let end = relative(arg(a, 2), len, len);
    let v = arg(a, 0);
    while k < end {
        put(vm, o, k, v)?;
        k += 1.0;
    }
    Ok(o)
}

/// `Array.prototype.copyWithin(target, start, end)`.
pub(crate) fn array_copy_within(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let mut to = relative(arg(a, 0), len, 0.0);
    let mut from = relative(arg(a, 1), len, 0.0);
    let end = relative(arg(a, 2), len, len);
    let mut count = (end - from).min(len - to);
    let dir = if from < to && to < from + count {
        from += count - 1.0;
        to += count - 1.0;
        -1.0
    } else {
        1.0
    };
    while count > 0.0 {
        let (has, v) = element(vm, o, from)?;
        if has {
            put(vm, o, to, v)?;
        } else {
            delete(vm, o, to)?;
        }
        from += dir;
        to += dir;
        count -= 1.0;
    }
    Ok(o)
}

/// `Array.of(…items)`.
fn array_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let n = a.len() as f64;
    let out = if crate::intrinsics::is_constructor(this) && !(this.is_cell() && as_object(this) == vm.array_fn) {
        crate::intrinsics::construct(vm, this, &[Value::number(n)])?
    } else {
        new_array_len(vm, n)?
    };
    for (i, &v) in a.iter().enumerate() {
        create(vm, out, i as f64, v)?;
    }
    set_length_or_throw(vm, out, n)?;
    Ok(out)
}

/// `Array.from(items, mapfn, thisArg)` over an array-like (no iterators
/// without symbols; a string's characters are taken as its iteration
/// would).
/// `Array.fromAsync(items, mapfn, thisArg)`: a promise of the array
/// `Array.from` would make, each value (and each mapped one) awaited. An
/// async JS helper, made on first use, runs the algorithm; what it calls
/// it gets from here, not from globals or prototypes code may have
/// replaced.
fn array_from_async(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if vm.from_async_helper.is_undefined() {
        let make = crate::builtins::run_source(vm, FROM_ASYNC)?;
        vm.from_async_helper = vm.handle_scope(|vm| {
            vm.handles.push(make);
            let mut natives = Vec::new();
            for (name, f, n) in [
                ("isConstructor", from_async_is_constructor as NativeFn, 1),
                ("call", from_async_call, 2),
                ("define", from_async_define, 3),
                ("typeError", from_async_type_error, 1),
                ("fromSync", from_async_from_sync, 1),
            ] {
                let f = object_value(vm.native_function(name, f, n, crate::jit::Ty::Any, false));
                vm.handles.push(f);
                natives.push(f);
            }
            let a_async = vm.intern(&format!("{}Symbol.asyncIterator", crate::symbol::SYMBOL_MARK));
            natives.push(crate::symbol::atom_symbol(vm, a_async).expect("Symbol.asyncIterator"));
            natives.push(crate::symbol::atom_symbol(vm, vm.a_sym_iterator).expect("Symbol.iterator"));
            call_function(vm, make, Value::UNDEFINED, &natives)
        })?;
    }
    let helper = vm.from_async_helper;
    call_function(vm, helper, Value::UNDEFINED, &[this, arg(a, 0), arg(a, 1), arg(a, 2)])
}

/// `Array.fromAsync`'s algorithm (ECMA-262 §23.1.2.2), its steps as the
/// spec orders them. `fromSync(v)` is a sync iterator's step as
/// AsyncFromSyncIterator takes it (the promise `for await` awaits).
const FROM_ASYNC: &str = r#"(function (isConstructor, call, define, typeError, fromSync, asyncIterator, iterator) {
  "use strict";
  function isObject(x) { return (typeof x === "object" && x !== null) || typeof x === "function"; }
  function getMethod(o, key) {
    const m = o[key];
    if (m === void 0 || m === null) return void 0;
    if (typeof m !== "function") typeError("Array.fromAsync: an iterator method is not a function");
    return m;
  }
  async function closeAsync(it, sync, e) {
    try {
      const ret = getMethod(it, "return");
      if (ret !== void 0) {
        const r = call(ret, it);
        if (sync) {
          if (isObject(r)) { r.done; await fromSync(r.value); }
        } else {
          await r;
        }
      }
    } catch (_) {}
    throw e;
  }
  return async function (C, items, mapfn, thisArg) {
    const mapping = mapfn !== void 0;
    if (mapping && typeof mapfn !== "function") typeError("Array.fromAsync: the mapper is not a function");
    const usingAsync = getMethod(items, asyncIterator);
    const usingSync = usingAsync === void 0 ? getMethod(items, iterator) : void 0;
    if (usingAsync !== void 0 || usingSync !== void 0) {
      const sync = usingAsync === void 0;
      const it = call(sync ? usingSync : usingAsync, items);
      if (!isObject(it)) typeError("Array.fromAsync: the iterator is not an object");
      const next = it.next;
      const A = isConstructor(C) ? new C() : [];
      for (let k = 0; ; k++) {
        let r = call(next, it);
        if (!sync) r = await r;
        if (!isObject(r)) typeError("Array.fromAsync: an iterator result is not an object");
        const done = !!r.done;
        let v;
        if (sync) {
          // (AsyncFromSyncIterator: the value awaited even when done; a
          // rejection closes the iterator if it isn't.)
          v = r.value;
          try {
            v = await fromSync(v);
          } catch (e) {
            if (!done) {
              try {
                const ret = getMethod(it, "return");
                if (ret !== void 0) call(ret, it);
              } catch (_) {}
            }
            throw e;
          }
        }
        if (done) {
          A.length = k;
          return A;
        }
        if (!sync) v = r.value;
        if (mapping) {
          try {
            v = await call(mapfn, thisArg, v, k);
          } catch (e) {
            return closeAsync(it, sync, e);
          }
        }
        try {
          define(A, k, v);
        } catch (e) {
          return closeAsync(it, sync, e);
        }
      }
    }
    // An array-like: its length, then each element awaited.
    let len = +items.length;
    len = len !== len || len <= 0 ? 0 : len === Infinity ? 9007199254740991 : len - len % 1;
    if (len > 9007199254740991) len = 9007199254740991;
    let A;
    if (isConstructor(C)) {
      A = new C(len);
    } else {
      A = [];
      A.length = len;
    }
    for (let k = 0; k < len; k++) {
      let v = await items[k];
      if (mapping) v = await call(mapfn, thisArg, v, k);
      define(A, k, v);
    }
    A.length = len;
    return A;
  };
})"#;

fn from_async_is_constructor(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(crate::intrinsics::is_constructor(arg(a, 0))))
}

/// `call(f, that, ...args)`: `f` called with `this` `that`.
fn from_async_call(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    call_function(vm, arg(a, 0), arg(a, 1), a.get(2..).unwrap_or(&[]))
}

/// `define(o, k, v)`: CreateDataPropertyOrThrow.
fn from_async_define(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let key = crate::intrinsics::to_property_key(vm, arg(a, 1))?;
    vm.create_data_property(arg(a, 0), key, arg(a, 2))?;
    Ok(Value::UNDEFINED)
}

fn from_async_type_error(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let m = to_string_units(arg(a, 0));
    Err(Error::type_error(String::from_utf16_lossy(&m)))
}

fn from_async_from_sync(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    crate::promise::async_from_sync(vm, arg(a, 0))
}

fn array_from(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let items = arg(a, 0);
    let mapfn = arg(a, 1);
    if !mapfn.is_undefined() && !is_callable(mapfn) {
        return Err(Error::type_error("Array.from: mapfn is not callable"));
    }
    let that = arg(a, 2);
    let custom = crate::intrinsics::is_constructor(this) && !(this.is_cell() && as_object(this) == vm.array_fn);
    // An iterable: its values (into `this`'s new object, if a constructor).
    if !items.is_undefined() && !items.is_null() {
        let a_iter = vm.a_sym_iterator;
        let m = vm.get(items, a_iter)?;
        if !m.is_undefined() && !m.is_null() && (custom || string_units(items).is_none()) {
            let out = if custom { crate::intrinsics::construct(vm, this, &[])? } else { vm.new_array() };
            let mut n = 0.0;
            crate::symbol::iterate(vm, items, |vm, v| {
                let v = if mapfn.is_undefined() { v } else { call_function(vm, mapfn, that, &[v, Value::number(n)])? };
                create(vm, out, n, v)?;
                n += 1.0;
                Ok(())
            })?;
            set_length_or_throw(vm, out, n)?;
            return Ok(out);
        }
    }
    if custom {
        let (o, len) = this_object(vm, items)?;
        let out = crate::intrinsics::construct(vm, this, &[Value::number(len)])?;
        let mut i = 0.0;
        while i < len {
            let v = get(vm, o, i)?;
            let v = if mapfn.is_undefined() { v } else { call_function(vm, mapfn, that, &[v, Value::number(i)])? };
            create(vm, out, i, v)?;
            i += 1.0;
        }
        set_length_or_throw(vm, out, len)?;
        return Ok(out);
    }
    let out = vm.new_array();
    if let Some(u) = string_units(items) {
        let mut n = 0.0;
        let mut i = 0;
        while i < u.len() {
            let c = u[i];
            let pair = (0xD800..0xDC00).contains(&c) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]);
            let s = if pair { vm.new_string(&u[i..i + 2]) } else { vm.single_char(c) };
            i += if pair { 2 } else { 1 };
            let v = if mapfn.is_undefined() { s } else { call_function(vm, mapfn, that, &[s, Value::number(n)])? };
            create(vm, out, n, v)?;
            n += 1.0;
        }
        set_length(vm, out, n)?;
        return Ok(out);
    }
    let (o, len) = this_object(vm, items)?;
    let mut i = 0.0;
    while i < len {
        let v = get(vm, o, i)?;
        let v = if mapfn.is_undefined() { v } else { call_function(vm, mapfn, that, &[v, Value::number(i)])? };
        create(vm, out, i, v)?;
        i += 1.0;
    }
    set_length(vm, out, len)?;
    Ok(out)
}

/// The arguments `f.apply(that, list)` passes: `list`'s elements (holes as
/// `undefined`; none for `undefined` or `null`): CreateListFromArrayLike.
pub(crate) fn apply_list(vm: &mut Vm, list: Value) -> Result<Vec<Value>> {
    if list.is_undefined() || list.is_null() {
        return Ok(Vec::new());
    }
    if !is_object(list) {
        return Err(Error::type_error("CreateListFromArrayLike called on non-object"));
    }
    let len = vm.length_of(list)?;
    if len > (1 << 24) as f64 {
        return Err(Error::range_error("Too many arguments in function call"));
    }
    let mut out = Vec::with_capacity(len as usize);
    let mut i = 0.0;
    while i < len {
        let v = get(vm, list, i)?;
        vm.handles.push(v);
        out.push(v);
        i += 1.0;
    }
    Ok(out)
}

/// `at(i)`: element `i`, counted from the end if negative.
pub(crate) fn array_at(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let n = to_integer(arg(a, 0));
    let k = if n < 0.0 { len + n } else { n };
    if k < 0.0 || k >= len {
        return Ok(Value::UNDEFINED);
    }
    get(vm, o, k)
}

/// `findLast` (`index`: `findLastIndex`): the last element the callback
/// accepts, or its index.
fn find_last(vm: &mut Vm, this: Value, a: &[Value], index: bool) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let f = callback(a, if index { "findLastIndex" } else { "findLast" })?;
    let that = arg(a, 1);
    let mut i = len - 1.0;
    while i >= 0.0 {
        let v = get(vm, o, i)?;
        let r = call_function(vm, f, that, &[v, Value::number(i), o])?;
        if to_boolean(r) {
            return Ok(if index { Value::number(i) } else { v });
        }
        i -= 1.0;
    }
    Ok(if index { Value::from_i32(-1) } else { Value::UNDEFINED })
}

pub(crate) fn array_find_last(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    find_last(vm, this, a, false)
}

pub(crate) fn array_find_last_index(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    find_last(vm, this, a, true)
}

/// A new array of `len` elements, element `i` from `f(i)`.
fn array_from_fn(vm: &mut Vm, len: f64, mut f: impl FnMut(&mut Vm, f64) -> Result<Value>) -> Result<Value> {
    if len > u32::MAX as f64 {
        return Err(Error::range_error("Invalid array length"));
    }
    let out = vm.new_array();
    let mut i = 0.0;
    while i < len {
        let v = f(vm, i)?;
        create(vm, out, i, v)?;
        i += 1.0;
    }
    Ok(out)
}

/// `toReversed()`: a reversed copy.
fn array_to_reversed(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    array_from_fn(vm, len, |vm, i| get(vm, o, len - 1.0 - i))
}

/// `toSorted(compare)`: a sorted copy.
fn array_to_sorted(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let cmp = arg(a, 0);
    if !cmp.is_undefined() && !is_callable(cmp) {
        return Err(Error::type_error("The comparison function must be either a function or undefined"));
    }
    let (o, len) = this_object(vm, this)?;
    let out = array_from_fn(vm, len, |vm, i| get(vm, o, i))?;
    array_sort(vm, out, &[cmp])
}

/// `toSpliced(start, skipCount, ...items)`: a copy with the elements
/// `splice` would remove replaced by the items.
fn array_to_spliced(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let start = relative(arg(a, 0), len, 0.0);
    let skip = match a.len() {
        0 => 0.0,
        1 => len - start,
        _ => to_integer(a[1]).clamp(0.0, len - start),
    };
    let items = a.get(2..).unwrap_or(&[]);
    let n = len + items.len() as f64 - skip;
    if n > 9007199254740991.0 {
        return Err(Error::type_error("Invalid array length"));
    }
    array_from_fn(vm, n, |vm, i| {
        if i < start {
            get(vm, o, i)
        } else if i < start + items.len() as f64 {
            Ok(items[(i - start) as usize])
        } else {
            get(vm, o, i - items.len() as f64 + skip)
        }
    })
}

/// `with(i, v)`: a copy with element `i` (counted from the end if
/// negative) `v`.
fn array_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (o, len) = this_object(vm, this)?;
    let n = to_integer(arg(a, 0));
    let k = if n < 0.0 { len + n } else { n };
    if k < 0.0 || k >= len {
        return Err(Error::range_error("Invalid index"));
    }
    let v = arg(a, 1);
    array_from_fn(vm, len, |vm, i| if i == k { Ok(v) } else { get(vm, o, i) })
}
