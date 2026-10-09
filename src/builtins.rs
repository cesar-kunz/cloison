//! Builtins beyond Octane's harness (see `Vm::new` for those): `String`
//! and its prototype's methods, more of `Math`, `parseInt` and friends,
//! `Error` and its kinds, and the `toString`/`valueOf` methods ToPrimitive
//! calls (`interp::to_primitive`). Installed by [`install`].

use std::borrow::Cow;

use crate::interp::{to_number, to_string_units};
use crate::jit::Ty;
use crate::object::{Class, DONT_ENUM, Object, READ_ONLY, DONT_DELETE, as_object, object_value};
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, number_to_string, string_units};

/// The kinds of error objects, in `Vm::error_prototypes` order. The
/// runtime's own errors (`Error::type_error`, …) are caught as objects of
/// the kind their message starts with.
pub(crate) const ERROR_KINDS: [&str; 7] = [
    "Error",
    "TypeError",
    "RangeError",
    "ReferenceError",
    "SyntaxError",
    "EvalError",
    "URIError",
];

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = vm.object_prototype;
    let sp = vm.string_prototype;
    let any = Ty::Any;

    // `String`, a conversion (no wrapper objects: `new String` isn't one).
    let string = vm.define_native(g, "String", string_fn, 1, any, false);
    link_constructor(vm, string, sp);
    vm.define_native(string, "fromCharCode", string_from_char_code, 1, any, false);
    for (name, f, n) in [
        ("charCodeAt", string_char_code_at as NativeFn, 1),
        ("charAt", string_char_at, 1),
        ("indexOf", string_index_of, 1),
        ("lastIndexOf", string_last_index_of, 1),
        ("substring", string_substring, 2),
        ("substr", string_substr, 2),
        ("slice", string_slice, 2),
        ("toLowerCase", string_to_lower_case, 0),
        ("toUpperCase", string_to_upper_case, 0),
        ("trim", string_trim, 0),
        ("split", string_split, 2),
        ("concat", string_concat, 1),
        ("toString", string_to_string, 0),
        ("valueOf", string_to_string, 0),
    ] {
        vm.define_native(sp, name, f, n, any, false);
    }

    let fp = vm.function_prototype;
    let apply = vm.define_native(fp, "apply", function_apply, 2, any, false);
    vm.function_apply = unsafe { (*apply).code };

    // The methods ToPrimitive calls.
    vm.define_native(op, "toString", object_to_string, 0, any, false);
    vm.define_native(op, "valueOf", object_value_of, 0, any, false);
    let ap = vm.array_prototype;
    vm.define_native(ap, "toString", array_to_string, 0, any, false);
    vm.define_native(ap, "join", array_join, 1, any, false);
    let np = vm.number_prototype;
    vm.define_native(np, "valueOf", number_value_of, 0, any, false);

    // More of `Math`.
    let a_math = vm.intern("Math");
    let math = vm.get(object_value(g), a_math).unwrap();
    let math = as_object(math);
    for (name, f, n) in [
        ("tan", math_tan as NativeFn, 1),
        ("atan", math_atan, 1),
        ("atan2", math_atan2, 2),
        ("asin", math_asin, 1),
        ("acos", math_acos, 1),
        ("imul", math_imul, 2),
    ] {
        vm.define_native(math, name, f, n, any, false);
    }

    for (name, f, n) in [
        ("parseInt", parse_int as NativeFn, 2),
        ("parseFloat", parse_float, 1),
        ("isNaN", is_nan, 1),
        ("isFinite", is_finite, 1),
    ] {
        vm.define_native(g, name, f, n, any, false);
    }

    // `Error` and its kinds, each prototype inheriting `Error.prototype`.
    const CTORS: [NativeFn; 7] = [
        error_ctor::<0>,
        error_ctor::<1>,
        error_ctor::<2>,
        error_ctor::<3>,
        error_ctor::<4>,
        error_ctor::<5>,
        error_ctor::<6>,
    ];
    let a_name = vm.intern("name");
    let a_message = vm.intern("message");
    for (k, kind) in ERROR_KINDS.iter().enumerate() {
        let parent = vm.error_prototypes.first().copied().unwrap_or(op);
        let proto = vm.new_object(object_value(parent));
        let c = vm.define_native(g, kind, CTORS[k], 1, any, true);
        link_constructor(vm, c, proto);
        let name = vm.new_string(&kind.encode_utf16().collect::<Vec<_>>());
        vm.add_property_with(proto, a_name, name, DONT_ENUM);
        let empty = vm.new_string(&[]);
        vm.add_property_with(proto, a_message, empty, DONT_ENUM);
        if k == 0 {
            vm.define_native(proto, "toString", error_to_string, 0, any, false);
        }
        vm.error_prototypes.push(proto);
    }
}

/// `c.prototype = proto` and `proto.constructor = c`, as the builtins'.
fn link_constructor(vm: &mut Vm, c: *mut Object, proto: *mut Object) {
    let (a_p, a_c) = (vm.intern("prototype"), vm.intern("constructor"));
    vm.add_property_with(c, a_p, object_value(proto), READ_ONLY | DONT_ENUM | DONT_DELETE);
    vm.add_property_with(proto, a_c, object_value(c), DONT_ENUM);
}

// ---- Helpers ----

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// ToString of `v`, borrowing a string's own units.
fn units<'a>(v: Value) -> Cow<'a, [u16]> {
    match string_units(v) {
        Some(u) => Cow::Borrowed(u),
        None => Cow::Owned(to_string_units(v)),
    }
}

/// `this` of a `String.prototype` method, as a string.
fn this_units<'a>(this: Value) -> Result<Cow<'a, [u16]>> {
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error(
            "String.prototype method called on null or undefined",
        ));
    }
    Ok(units(this))
}

/// ToIntegerOrInfinity of argument `i` (0 if missing or NaN).
fn int_arg(a: &[Value], i: usize) -> f64 {
    match a.get(i) {
        Some(&v) if v.is_int32() => v.as_i32_unchecked() as f64,
        Some(&v) => {
            let d = to_number(v);
            if d.is_nan() { 0.0 } else { d.trunc() }
        }
        None => 0.0,
    }
}

/// A relative index (`slice`'s): negative counts from `len`; clamped to
/// `0..=len`.
fn relative(d: f64, len: usize) -> usize {
    if d < 0.0 {
        (len as f64 + d).max(0.0) as usize
    } else {
        d.min(len as f64) as usize
    }
}

fn string(vm: &mut Vm, u: &[u16]) -> Value {
    if let [c] = u {
        return vm.single_char(*c);
    }
    vm.new_string(u)
}

fn str_value(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

fn num(d: f64) -> Value {
    Value::number(d)
}

// ---- String ----

/// `String(v)`: ToString (`""` without an argument).
fn string_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    match a.first() {
        None => Ok(vm.new_string(&[])),
        Some(&v) if string_units(v).is_some() => Ok(v),
        Some(&v) => {
            let u = to_string_units(v);
            Ok(vm.new_string(&u))
        }
    }
}

/// `String.fromCharCode(…codes)`: each ToUint16.
fn string_from_char_code(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let u: Vec<u16> = a
        .iter()
        .map(|&v| {
            if v.is_int32() {
                v.as_i32_unchecked() as u16
            } else {
                crate::interp::to_int32(to_number(v)) as u16
            }
        })
        .collect();
    Ok(string(vm, &u))
}

fn string_char_code_at(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let i = int_arg(a, 0);
    Ok(if i >= 0.0 && (i as usize) < u.len() {
        Value::from_i32(u[i as usize] as i32)
    } else {
        Value::from_f64(f64::NAN)
    })
}

fn string_char_at(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let i = int_arg(a, 0);
    Ok(if i >= 0.0 && (i as usize) < u.len() {
        vm.single_char(u[i as usize])
    } else {
        vm.new_string(&[])
    })
}

fn find(hay: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    if needle.is_empty() {
        return Some(from.min(hay.len()));
    }
    (from..=hay.len().saturating_sub(needle.len()))
        .find(|&i| i + needle.len() <= hay.len() && hay[i..i + needle.len()] == *needle)
}

fn string_index_of(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let needle = units(arg(a, 0));
    let from = int_arg(a, 1).clamp(0.0, u.len() as f64) as usize;
    Ok(Value::from_i32(find(&u, &needle, from).map_or(-1, |i| i as i32)))
}

fn string_last_index_of(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let needle = units(arg(a, 0));
    let pos = match a.get(1).map(|&v| to_number(v)) {
        Some(d) if !d.is_nan() => d.trunc().clamp(0.0, u.len() as f64) as usize,
        _ => u.len(),
    };
    if needle.len() > u.len() {
        return Ok(Value::from_i32(-1));
    }
    let start = pos.min(u.len() - needle.len());
    let r = (0..=start).rev().find(|&i| u[i..i + needle.len()] == *needle);
    Ok(Value::from_i32(r.map_or(-1, |i| i as i32)))
}

fn string_substring(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let len = u.len() as f64;
    let s = int_arg(a, 0).clamp(0.0, len) as usize;
    let e = match a.get(1) {
        Some(v) if !v.is_undefined() => int_arg(a, 1).clamp(0.0, len) as usize,
        _ => u.len(),
    };
    let (s, e) = (s.min(e), s.max(e));
    Ok(string(vm, &u[s..e]))
}

fn string_substr(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = relative(int_arg(a, 0), u.len());
    let n = match a.get(1) {
        Some(v) if !v.is_undefined() => int_arg(a, 1).clamp(0.0, (u.len() - s) as f64) as usize,
        _ => u.len() - s,
    };
    Ok(string(vm, &u[s..s + n]))
}

fn string_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = relative(int_arg(a, 0), u.len());
    let e = match a.get(1) {
        Some(v) if !v.is_undefined() => relative(int_arg(a, 1), u.len()),
        _ => u.len(),
    };
    Ok(string(vm, if s < e { &u[s..e] } else { &[] }))
}

fn string_to_lower_case(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let s = String::from_utf16_lossy(&this_units(this)?).to_lowercase();
    Ok(str_value(vm, &s))
}

fn string_to_upper_case(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let s = String::from_utf16_lossy(&this_units(this)?).to_uppercase();
    Ok(str_value(vm, &s))
}

/// JavaScript's whitespace and line terminators.
fn is_space(c: u16) -> bool {
    matches!(
        c,
        0x09..=0x0D | 0x20 | 0xA0 | 0x1680 | 0x2000..=0x200A | 0x2028 | 0x2029 | 0x202F | 0x205F
            | 0x3000 | 0xFEFF
    )
}

fn string_trim(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = u.iter().position(|&c| !is_space(c)).unwrap_or(u.len());
    let e = u.iter().rposition(|&c| !is_space(c)).map_or(s, |e| e + 1);
    Ok(string(vm, &u[s..e]))
}

/// `split(sep, limit)` with a string separator (or none: the whole
/// string; `""`: each code unit).
fn string_split(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?.into_owned();
    let limit = match a.get(1) {
        Some(v) if !v.is_undefined() => crate::interp::to_int32(to_number(*v)) as u32 as usize,
        _ => usize::MAX,
    };
    let mut parts: Vec<Value> = Vec::new();
    match a.first() {
        Some(v) if !v.is_undefined() => {
            let sep = units(*v).into_owned();
            if sep.is_empty() {
                for &c in u.iter().take(limit) {
                    parts.push(vm.single_char(c));
                }
            } else {
                let mut start = 0;
                while parts.len() < limit {
                    match find(&u, &sep, start) {
                        Some(i) => {
                            parts.push(string(vm, &u[start..i]));
                            start = i + sep.len();
                        }
                        None => {
                            parts.push(string(vm, &u[start..]));
                            break;
                        }
                    }
                }
            }
        }
        _ if limit > 0 => parts.push(this_string(vm, this, &u)),
        _ => {}
    }
    let arr = vm.new_array();
    crate::vm::array_push(vm, arr, &parts)?;
    Ok(arr)
}

/// `this` as a string value (itself if one).
fn this_string(vm: &mut Vm, this: Value, u: &[u16]) -> Value {
    if string_units(this).is_some() { this } else { vm.new_string(u) }
}

fn string_concat(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let mut u = this_units(this)?.into_owned();
    for &v in a {
        u.extend_from_slice(&units(v));
    }
    Ok(vm.new_string(&u))
}

fn string_to_string(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    match string_units(this) {
        Some(_) => Ok(this),
        None => Err(Error::type_error("String.prototype.toString called on a non-string")),
    }
}

// ---- Function.prototype.apply ----

/// The arguments `f.apply(that, list)` passes: `list`'s elements (holes as
/// `undefined`; none for `undefined` or `null`). `arguments` objects are
/// arrays.
pub(crate) fn apply_list(list: Value) -> Result<Vec<Value>> {
    if list.is_undefined() || list.is_null() {
        return Ok(Vec::new());
    }
    if !list.is_cell() || unsafe { (*as_object(list)).class } != Class::Array {
        return Err(Error::type_error("CreateListFromArrayLike called on a non-array"));
    }
    let o = as_object(list);
    let n = unsafe { (*o).elems_len } as usize;
    Ok((0..n)
        .map(|i| {
            let e = unsafe { *(*o).elems.add(i) };
            if e == Value::EMPTY { Value::UNDEFINED } else { e }
        })
        .collect())
}

/// `Function.prototype.apply` on a native (a call through it to a bytecode
/// function is unwrapped where it is made: `interp::unwrap_apply`).
fn function_apply(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let list = apply_list(arg(a, 1))?;
    if this.is_cell() && unsafe { (*as_object(this)).class } == Class::Native {
        return vm.call_other(this, arg(a, 0), &list);
    }
    Err(Error::type_error(format!("{this:?} is not a native function (apply it directly)")))
}

/// A new array of `values`: an `arguments` object.
pub(crate) fn arguments_object(vm: &mut Vm, values: &[Value]) -> Value {
    let arr = vm.new_array();
    crate::vm::array_push(vm, arr, values).expect("an array");
    arr
}

// ---- What ToPrimitive calls ----

/// `Object.prototype.toString`: `[object Array]`, `[object Object]`, ….
fn object_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let tag = if this.is_undefined() {
        "Undefined"
    } else if this.is_null() {
        "Null"
    } else if this.is_cell() {
        match unsafe { (*as_object(this)).class } {
            Class::Array => "Array",
            Class::Function | Class::Native => "Function",
            Class::String => "String",
            _ if crate::vm::date_value(this).is_some() => "Date",
            _ => "Object",
        }
    } else if this.is_bool() {
        "Boolean"
    } else {
        "Number"
    };
    Ok(str_value(vm, &format!("[object {tag}]")))
}

fn object_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(this)
}

fn number_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if this.is_number() {
        Ok(this)
    } else {
        Err(Error::type_error("Number.prototype.valueOf called on a non-number"))
    }
}

/// `Array.prototype.join(sep)`: elements' strings (holes, `undefined` and
/// `null` empty), separated by `sep` (`,` by default).
fn array_join(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !this.is_cell() || unsafe { (*as_object(this)).class } != Class::Array {
        return Err(Error::unsupported("Array.prototype.join on a non-array"));
    }
    let sep = match a.first() {
        Some(v) if !v.is_undefined() => units(*v).into_owned(),
        _ => vec![b',' as u16],
    };
    let o = as_object(this);
    let n = unsafe { (*o).elems_len } as usize;
    let mut out = Vec::new();
    for i in 0..n {
        if i > 0 {
            out.extend_from_slice(&sep);
        }
        let e = unsafe { *(*o).elems.add(i) };
        if e != Value::EMPTY && !e.is_undefined() && !e.is_null() {
            out.extend_from_slice(&units(e));
        }
    }
    Ok(vm.new_string(&out))
}

fn array_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    array_join(vm, this, &[])
}

// ---- Math ----

fn math_arg(a: &[Value], i: usize) -> f64 {
    a.get(i).map_or(f64::NAN, |&v| to_number(v))
}
fn math_tan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).tan()))
}
fn math_atan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).atan()))
}
fn math_atan2(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).atan2(math_arg(a, 1))))
}
fn math_asin(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).asin()))
}
fn math_acos(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).acos()))
}
fn math_imul(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = crate::interp::to_int32(math_arg(a, 0));
    let y = crate::interp::to_int32(math_arg(a, 1));
    Ok(Value::from_i32(x.wrapping_mul(y)))
}

// ---- Numbers from strings ----

/// `parseInt(s, radix)`: the longest prefix of digits in `radix` (2 to 36;
/// 0 or missing: 10, or 16 after `0x`), after whitespace and a sign.
fn parse_int(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    let radix = a.get(1).map_or(0, |&r| crate::interp::to_int32(to_number(r)));
    // An int32 in radix 10 is itself (box2d's `parseInt(count)`).
    if v.is_int32() && matches!(radix, 0 | 10) {
        return Ok(v);
    }
    let u = units(v);
    let mut i = u.iter().position(|&c| !is_space(c)).unwrap_or(u.len());
    let mut neg = false;
    if i < u.len() && (u[i] == b'-' as u16 || u[i] == b'+' as u16) {
        neg = u[i] == b'-' as u16;
        i += 1;
    }
    let mut radix = radix as u32;
    let hex_prefix = i + 1 < u.len() && u[i] == b'0' as u16 && (u[i + 1] | 0x20) == b'x' as u16;
    if radix == 0 {
        radix = 10;
        if hex_prefix {
            radix = 16;
            i += 2;
        }
    } else if radix == 16 {
        if hex_prefix {
            i += 2;
        }
    } else if !(2..=36).contains(&radix) {
        return Ok(Value::from_f64(f64::NAN));
    }
    let digit = |c: u16| char::from_u32(c as u32).and_then(|c| c.to_digit(radix));
    let start = i;
    let mut x = 0.0f64;
    while i < u.len() {
        let Some(d) = digit(u[i]) else { break };
        x = x * radix as f64 + d as f64;
        i += 1;
    }
    if i == start {
        return Ok(Value::from_f64(f64::NAN));
    }
    // Decimal digits past 2^53: the nearest double, as the string says.
    if radix == 10 && x >= 9007199254740992.0 {
        let s = String::from_utf16_lossy(&u[start..i]);
        x = s.parse().unwrap_or(x);
    }
    Ok(num(if neg { -x } else { x }))
}

/// `parseFloat(s)`: the longest prefix that is a decimal literal (or
/// `Infinity`), after whitespace.
fn parse_float(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    if v.is_number() {
        return Ok(v);
    }
    let u = units(v);
    let s = String::from_utf16_lossy(&u);
    let t = s.trim_start_matches(|c: char| c.len_utf16() == 1 && is_space(c as u16));
    let b = t.as_bytes();
    let mut i = 0;
    if i < b.len() && matches!(b[i], b'+' | b'-') {
        i += 1;
    }
    if t[i..].starts_with("Infinity") {
        return Ok(Value::from_f64(if b.first() == Some(&b'-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        }));
    }
    let digits = |i: &mut usize| {
        let s = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - s
    };
    let mut n = digits(&mut i);
    if i < b.len() && b[i] == b'.' {
        let dot = i;
        i += 1;
        let f = digits(&mut i);
        if n == 0 && f == 0 {
            i = dot;
        }
        n += f;
    }
    if n == 0 {
        return Ok(Value::from_f64(f64::NAN));
    }
    if i < b.len() && (b[i] | 0x20) == b'e' {
        let mut j = i + 1;
        if j < b.len() && matches!(b[j], b'+' | b'-') {
            j += 1;
        }
        if digits(&mut j) > 0 {
            i = j;
        }
    }
    Ok(num(t[..i].parse::<f64>().unwrap_or(f64::NAN)))
}

fn is_nan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(math_arg(a, 0).is_nan()))
}

fn is_finite(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(math_arg(a, 0).is_finite()))
}

// ---- Errors ----

/// `new Error(message)` (or a call) for kind `K` of [`ERROR_KINDS`].
fn error_ctor<const K: usize>(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let message = match a.first() {
        Some(v) if !v.is_undefined() => Some(units(*v).into_owned()),
        _ => None,
    };
    Ok(make_error(vm, K, message.as_deref()))
}

/// A new error object of kind `k`, with its own `message` if given.
pub(crate) fn make_error(vm: &mut Vm, k: usize, message: Option<&[u16]>) -> Value {
    let proto = vm.error_prototypes[k];
    let o = vm.new_object(object_value(proto));
    if let Some(m) = message {
        let a = vm.intern("message");
        let m = vm.new_string(m);
        vm.add_property_with(o, a, m, DONT_ENUM);
    }
    object_value(o)
}

/// The value a `catch` gets for a runtime error `e`: an error object of
/// the kind its message starts with (`TypeError: …`), or the message.
pub(crate) fn caught(vm: &mut Vm, e: &Error) -> Value {
    if let Some((kind, message)) = e.0.split_once(": ")
        && let Some(k) = ERROR_KINDS.iter().position(|&n| n == kind)
        && !vm.error_prototypes.is_empty()
    {
        let m: Vec<u16> = message.encode_utf16().collect();
        return make_error(vm, k, Some(&m));
    }
    str_value(vm, &e.0)
}

/// `Error.prototype.toString`: `name: message`, or either alone if the
/// other is empty.
fn error_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if !this.is_cell() {
        return Err(Error::type_error("Error.prototype.toString called on a non-object"));
    }
    let (a_name, a_message) = (vm.intern("name"), vm.intern("message"));
    let name = vm.get(this, a_name)?;
    let message = vm.get(this, a_message)?;
    let name = if name.is_undefined() { "Error".encode_utf16().collect() } else { units(name).into_owned() };
    let message = if message.is_undefined() { Vec::new() } else { units(message).into_owned() };
    let mut out = name;
    if !out.is_empty() && !message.is_empty() {
        out.extend(": ".encode_utf16());
    }
    out.extend_from_slice(&message);
    Ok(vm.new_string(&out))
}

// ---- Number.prototype.toString(radix) ----

/// `d` in `radix` (2 to 36), as V8's `DoubleToRadixCString`: the integer
/// part exactly, the fraction to as many digits as tell `d` apart from
/// its neighbors.
pub(crate) fn radix_string(d: f64, radix: u32) -> String {
    if d.is_nan() {
        return "NaN".into();
    }
    if d.is_infinite() {
        return if d < 0.0 { "-Infinity" } else { "Infinity" }.into();
    }
    if radix == 10 {
        return number_to_string(d);
    }
    let chars = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let neg = d < 0.0;
    let value = d.abs();
    let mut integer = value.floor();
    let mut fraction = value - integer;
    // Half the distance to the next double, at least the smallest one.
    let next = f64::from_bits(value.to_bits() + 1);
    let mut delta = (0.5 * (next - value)).max(f64::from_bits(1));
    let mut frac_digits: Vec<u8> = Vec::new();
    if fraction >= delta {
        loop {
            fraction *= radix as f64;
            delta *= radix as f64;
            let digit = fraction as u32;
            frac_digits.push(digit as u8);
            fraction -= digit as f64;
            if (fraction > 0.5 || (fraction == 0.5 && digit & 1 == 1)) && fraction + delta > 1.0 {
                // Round up, carrying into the integer part if need be.
                loop {
                    match frac_digits.pop() {
                        None => {
                            integer += 1.0;
                            break;
                        }
                        Some(d) if (d as u32) + 1 < radix => {
                            frac_digits.push(d + 1);
                            break;
                        }
                        Some(_) => {}
                    }
                }
                break;
            }
            if fraction < delta {
                break;
            }
        }
    }
    let mut int_digits: Vec<u8> = Vec::new();
    // Past 2^53 the low digits are zeros.
    while integer / radix as f64 >= 9007199254740992.0 {
        integer /= radix as f64;
        int_digits.push(b'0');
    }
    loop {
        let r = integer % radix as f64;
        int_digits.push(chars[r as usize]);
        integer = (integer - r) / radix as f64;
        if integer <= 0.0 {
            break;
        }
    }
    let mut s = String::new();
    if neg {
        s.push('-');
    }
    s.extend(int_digits.iter().rev().map(|&c| c as char));
    if !frac_digits.is_empty() {
        s.push('.');
        s.extend(frac_digits.iter().map(|&d| chars[d as usize] as char));
    }
    s
}
