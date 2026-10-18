//! Builtins beyond Octane's harness (see `Vm::new` for those): `String`
//! and its prototype's methods, more of `Math`, `parseInt` and friends,
//! `Error` and its kinds, and the `toString`/`valueOf` methods ToPrimitive
//! calls (`interp::to_primitive`). Installed by [`install`].

use std::borrow::Cow;

use crate::interp::{to_number, to_string_units};
use crate::jit::Ty;
use crate::object::{Class, DONT_DELETE, DONT_ENUM, Object, READ_ONLY, TypedKind, as_object, object_value};
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, number_to_string, string_units, wrapped};

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
    let string = vm.define_native(g, "String", string_fn, 1, any, true);
    link_constructor(vm, string, sp);
    vm.from_char_code_fn = vm.define_native(string, "fromCharCode", string_from_char_code, 1, any, false);
    vm.char_code_at_fn = vm.define_native(sp, "charCodeAt", string_char_code_at, 1, any, false);
    vm.define_natives(
        sp,
        &[
            ("charAt", string_char_at as NativeFn, 1),
            ("indexOf", string_index_of, 1),
            ("lastIndexOf", string_last_index_of, 1),
            ("substring", string_substring, 2),
            ("substr", string_substr, 2),
            ("slice", string_slice, 2),
            ("toLowerCase", string_to_lower_case, 0),
            ("toUpperCase", string_to_upper_case, 0),
        ("toLocaleLowerCase", string_to_lower_case, 0),
        ("toLocaleUpperCase", string_to_upper_case, 0),
            ("trim", string_trim, 0),
            ("split", string_split, 2),
            ("match", string_match, 1),
            ("replace", string_replace, 2),
            ("replaceAll", string_replace_all, 2),
            ("matchAll", string_match_all, 1),
            ("search", string_search, 1),
            ("concat", string_concat, 1),
            ("toString", string_to_string, 0),
            ("valueOf", string_to_string, 0),
            ("localeCompare", string_locale_compare, 1),
            ("startsWith", string_starts_with, 1),
            ("endsWith", string_ends_with, 1),
            ("includes", string_includes, 1),
            ("repeat", string_repeat, 1),
            ("padStart", string_pad_start, 1),
            ("padEnd", string_pad_end, 1),
            ("trimStart", string_trim_start, 0),
            ("trimEnd", string_trim_end, 0),
            ("codePointAt", string_code_point_at, 1),
            ("at", string_at, 1),
            ("normalize", string_normalize, 0),
            ("anchor", string_html::<0>, 1),
            ("big", string_html::<1>, 0),
            ("blink", string_html::<2>, 0),
            ("bold", string_html::<3>, 0),
            ("fixed", string_html::<4>, 0),
            ("fontcolor", string_html::<5>, 1),
            ("fontsize", string_html::<6>, 1),
            ("italics", string_html::<7>, 0),
            ("link", string_html::<8>, 1),
            ("small", string_html::<9>, 0),
            ("strike", string_html::<10>, 0),
            ("sub", string_html::<11>, 0),
            ("sup", string_html::<12>, 0),
        ],
    );
    vm.define_native(string, "fromCodePoint", string_from_code_point, 1, any, false);
    vm.define_native(string, "raw", string_raw, 1, any, false);
    let a_global_this = vm.intern("globalThis");
    vm.add_property_with(g, a_global_this, object_value(g), DONT_ENUM);
    let a_trim_left = vm.intern("trimLeft");
    let a_trim_right = vm.intern("trimRight");
    let a_trim_start = vm.intern("trimStart");
    let a_trim_end = vm.intern("trimEnd");
    let (ts, te) = (vm.get(object_value(sp), a_trim_start).unwrap(), vm.get(object_value(sp), a_trim_end).unwrap());
    vm.add_property_with(sp, a_trim_left, ts, DONT_ENUM);
    vm.add_property_with(sp, a_trim_right, te, DONT_ENUM);

    let fp = vm.function_prototype;
    let function = vm.define_native(g, "Function", function_ctor, 1, any, true);
    vm.eval_function = vm.define_native(g, "eval", eval_fn, 1, any, false);
    // A shell's (as V8's `d8`, which Octane's emscripten code expects).
    vm.define_native(g, "print", print_fn, 1, any, false);
    vm.define_native(g, "read", read_fn, 1, any, false);
    link_constructor(vm, function, fp);
    let apply = vm.define_native(fp, "apply", function_apply, 2, any, false);
    vm.define_native(fp, "bind", function_bind, 1, any, false);
    vm.define_native(fp, "toString", function_to_string, 0, any, false);
    vm.function_apply = unsafe { (*apply).code };

    // `Number`, a conversion, and its constants.
    let number = vm.define_native(g, "Number", number_fn, 1, any, true);
    link_constructor(vm, number, np_of(vm));
    for (name, v) in [
        ("MAX_VALUE", f64::MAX),
        ("MIN_VALUE", 5e-324),
        ("POSITIVE_INFINITY", f64::INFINITY),
        ("NEGATIVE_INFINITY", f64::NEG_INFINITY),
        ("NaN", f64::NAN),
        ("EPSILON", f64::EPSILON),
    ] {
        let a = vm.intern(name);
        vm.add_property_with(number, a, Value::from_f64(v), READ_ONLY | DONT_ENUM | DONT_DELETE);
    }
    for (name, v) in [("MAX_SAFE_INTEGER", 9007199254740991.0), ("MIN_SAFE_INTEGER", -9007199254740991.0)] {
        let a = vm.intern(name);
        vm.add_property_with(number, a, Value::from_f64(v), READ_ONLY | DONT_ENUM | DONT_DELETE);
    }
    vm.define_natives(
        number,
        &[
            ("isInteger", number_is_integer as NativeFn, 1),
            ("isSafeInteger", number_is_safe_integer, 1),
            ("isFinite", number_is_finite, 1),
            ("isNaN", number_is_nan, 1),
        ],
    );

    // `Boolean`, a conversion; booleans' methods.
    let bp = vm.new_object_with(Class::Wrapper, object_value(op), 4);
    unsafe { (*bp).code = Value::FALSE.to_bits() as *const crate::vm::Code };
    vm.boolean_prototype = bp;
    let boolean = vm.define_native(g, "Boolean", boolean_fn, 1, any, true);
    link_constructor(vm, boolean, bp);
    vm.define_native(bp, "toString", boolean_to_string, 0, any, false);
    vm.define_native(bp, "valueOf", boolean_value_of, 0, any, false);

    crate::props::install(vm);

    // The methods ToPrimitive calls.
    vm.define_native(op, "toString", object_to_string, 0, any, false);
    vm.define_native(op, "valueOf", object_value_of, 0, any, false);
    crate::array::install(vm);
    let np = vm.number_prototype;
    vm.define_native(np, "toExponential", number_to_exponential, 1, any, false);
    vm.define_native(np, "toLocaleString", number_to_locale_string, 0, any, false);
    let np = vm.number_prototype;
    vm.define_native(np, "valueOf", number_value_of, 0, any, false);

    // More of `Math`.
    let a_math = vm.intern("Math");
    let math = vm.get(object_value(g), a_math).unwrap();
    let math = as_object(math);
    vm.define_natives(
        math,
        &[
            ("tan", math_tan as NativeFn, 1),
            ("atan", math_atan, 1),
            ("atan2", math_atan2, 2),
            ("asin", math_asin, 1),
            ("acos", math_acos, 1),
            ("imul", math_imul, 2),
            ("cbrt", math_cbrt, 1),
            ("clz32", math_clz32, 1),
            ("cosh", math_cosh, 1),
            ("sinh", math_sinh, 1),
            ("tanh", math_tanh, 1),
            ("acosh", math_acosh, 1),
            ("asinh", math_asinh, 1),
            ("atanh", math_atanh, 1),
            ("expm1", math_expm1, 1),
            ("log1p", math_log1p, 1),
            ("log2", math_log2, 1),
            ("log10", math_log10, 1),
            ("sign", math_sign, 1),
            ("trunc", math_trunc, 1),
            ("fround", math_fround, 1),
            ("hypot", math_hypot, 2),
        ],
    );
    let a = vm.intern("SQRT1_2");
    vm.add_property_with(math, a, Value::from_f64(std::f64::consts::FRAC_1_SQRT_2), READ_ONLY | DONT_ENUM | DONT_DELETE);

    vm.define_natives(
        g,
        &[
            ("parseInt", parse_int as NativeFn, 2),
            ("parseFloat", parse_float, 1),
            ("isNaN", is_nan, 1),
            ("isFinite", is_finite, 1),
            ("escape", escape_fn, 1),
            ("unescape", unescape_fn, 1),
            ("encodeURIComponent", encode_uri_component, 1),
            ("encodeURI", encode_uri, 1),
            ("decodeURIComponent", decode_uri_component, 1),
            ("decodeURI", decode_uri, 1),
        ],
    );
    // `Number.parseInt` and `parseFloat` are the globals themselves.
    for name in ["parseInt", "parseFloat"] {
        let a = vm.intern(name);
        let f = vm.get(object_value(g), a).unwrap();
        vm.add_property_with(number, a, f, DONT_ENUM);
    }
    // A function's `caller` and `arguments`: accessors that throw (as
    // strict functions' do; sloppy ones' don't exist here).
    let thrower = vm.native_function("", throw_type_error, 0, any, false);
    vm.thrower = thrower;
    // (%ThrowTypeError% is frozen.)
    vm.seal(thrower, true);
    for name in ["caller", "arguments"] {
        let a = vm.intern(name);
        vm.define_accessor(fp, a, Some(object_value(thrower)), Some(object_value(thrower)), DONT_ENUM);
    }

    crate::date::install(vm);
    // The test262 host hooks (`$262`), as a shell has them.
    let host = vm.new_object(object_value(op));
    let a_host = vm.intern("$262");
    vm.add_property_with(g, a_host, object_value(host), DONT_ENUM);
    let a_global = vm.intern("global");
    vm.add_property_with(host, a_global, object_value(g), DONT_ENUM);
    vm.define_natives(
        host,
        &[
            ("evalScript", eval_script as NativeFn, 1),
            ("gc", host_gc, 0),
            ("detachArrayBuffer", host_detach_array_buffer, 1),
        ],
    );
    install_typed_arrays(vm);
    crate::collections::install(vm);
    install_regexps(vm);

    crate::json::install(vm);

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
    let mut error_ctor = Value::UNDEFINED;
    for (k, kind) in ERROR_KINDS.iter().enumerate() {
        let parent = vm.error_prototypes.first().copied().unwrap_or(op);
        let proto = vm.new_object(object_value(parent));
        let c = vm.define_native(g, kind, CTORS[k], 1, any, true);
        link_constructor(vm, c, proto);
        // (The kinds' constructors inherit `Error`.)
        if k == 0 {
            error_ctor = object_value(c);
        } else {
            vm.set_prototype_of(c, error_ctor);
        }
        let name = vm.new_string(&kind.encode_utf16().collect::<Vec<_>>());
        vm.add_property_with(proto, a_name, name, DONT_ENUM);
        let empty = vm.new_string(&[]);
        vm.add_property_with(proto, a_message, empty, DONT_ENUM);
        if k == 0 {
            vm.define_native(proto, "toString", error_to_string, 0, any, false);
        }
        vm.error_prototypes.push(proto);
    }

    // (After the errors: `AggregateError` inherits `Error.prototype`.)
    crate::promise::install(vm);
    crate::reflect::install(vm);
    crate::proxy::install(vm);
    crate::symbol::install(vm);
    crate::generator::install(vm);
    // (`Symbol.prototype`'s is called directly: a symbol is a primitive.)
    vm.to_primitive_defined = false;
}

/// `c.prototype = proto` and `proto.constructor = c`, as the builtins'.
pub(crate) fn link_constructor(vm: &mut Vm, c: *mut Object, proto: *mut Object) {
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
    if let Some(u) = wrapped(this).and_then(string_units) {
        return Ok(Cow::Borrowed(u));
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
    let construct = vm.is_construct();
    let s = match a.first() {
        None => vm.new_string(&[]),
        Some(&v) if string_units(v).is_some() => v,
        // (`String(symbol)` is its description; `new String(symbol)` throws.)
        Some(&v) if crate::symbol::is_symbol(v) && !construct => {
            let u = crate::symbol::symbol_string(v);
            return Ok(vm.new_string(&u));
        }
        Some(&v) => {
            let u = to_string_units(v);
            if vm.conv_thrown {
                return Err(vm.take_conv_error());
            }
            vm.new_string(&u)
        }
    };
    Ok(if construct { vm.new_wrapper(s) } else { s })
}

/// `String.fromCharCode(…codes)`: each ToUint16.
fn string_from_char_code(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if let [v] = a
        && v.is_int32()
    {
        return Ok(vm.single_char(v.as_i32_unchecked() as u16));
    }
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
    crate::regexp::find(hay, needle, from)
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

fn string_trim_start(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = u.iter().position(|&c| !is_space(c)).unwrap_or(u.len());
    Ok(string(vm, &u[s..]))
}

fn string_trim_end(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let e = u.iter().rposition(|&c| !is_space(c)).map_or(0, |e| e + 1);
    Ok(string(vm, &u[..e]))
}

/// `localeCompare(that)`: by code units (no locales).
fn string_locale_compare(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let v = units(arg(a, 0));
    Ok(Value::from_i32(match u.cmp(&v) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    }))
}

/// The search string of `startsWith` and the like: a `TypeError` for a
/// regular expression.
fn search_string<'a>(v: Value, what: &str) -> Result<Cow<'a, [u16]>> {
    if regexp_of(v).is_some() {
        return Err(Error::type_error(format!("First argument to String.prototype.{what} must not be a regular expression")));
    }
    Ok(units(v))
}

fn string_starts_with(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = search_string(arg(a, 0), "startsWith")?;
    let at = int_arg(a, 1).clamp(0.0, u.len() as f64) as usize;
    Ok(Value::from_bool(u[at..].starts_with(&s)))
}

fn string_ends_with(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = search_string(arg(a, 0), "endsWith")?;
    let end = match a.get(1) {
        Some(v) if !v.is_undefined() => int_arg(a, 1).clamp(0.0, u.len() as f64) as usize,
        _ => u.len(),
    };
    Ok(Value::from_bool(u[..end].ends_with(&s)))
}

fn string_includes(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let s = search_string(arg(a, 0), "includes")?;
    let at = int_arg(a, 1).clamp(0.0, u.len() as f64) as usize;
    Ok(Value::from_bool(find(&u, &s, at).is_some()))
}

fn string_repeat(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let n = int_arg(a, 0);
    if n < 0.0 || n == f64::INFINITY {
        return Err(Error::range_error("Invalid count value"));
    }
    if n * u.len() as f64 > (1u64 << 30) as f64 {
        return Err(Error::range_error("Invalid string length"));
    }
    Ok(vm.new_string(&u.repeat(n as usize)))
}

/// `padStart` (`start`) and `padEnd`.
fn pad(vm: &mut Vm, this: Value, a: &[Value], start: bool) -> Result<Value> {
    let u = this_units(this)?;
    let max = int_arg(a, 0);
    if max <= u.len() as f64 {
        return Ok(this_string(vm, this, &u));
    }
    let filler = match a.get(1) {
        Some(v) if !v.is_undefined() => units(*v).into_owned(),
        _ => vec![b' ' as u16],
    };
    if filler.is_empty() {
        return Ok(this_string(vm, this, &u));
    }
    if max > (1u64 << 30) as f64 {
        return Err(Error::range_error("Invalid string length"));
    }
    let fill_len = max as usize - u.len();
    let mut f: Vec<u16> = filler.iter().cycle().take(fill_len).copied().collect();
    let out = if start {
        f.extend_from_slice(&u);
        f
    } else {
        let mut o = u.into_owned();
        o.extend(f);
        o
    };
    Ok(vm.new_string(&out))
}

fn string_pad_start(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    pad(vm, this, a, true)
}

fn string_pad_end(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    pad(vm, this, a, false)
}

fn string_code_point_at(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let i = int_arg(a, 0);
    if i < 0.0 || i >= u.len() as f64 {
        return Ok(Value::UNDEFINED);
    }
    let i = i as usize;
    let c = u[i];
    if (0xD800..0xDC00).contains(&c) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) {
        let cp = 0x10000 + ((c as i32 - 0xD800) << 10) + (u[i + 1] as i32 - 0xDC00);
        return Ok(Value::from_i32(cp));
    }
    Ok(Value::from_i32(c as i32))
}

fn string_at(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    let i = int_arg(a, 0);
    let i = if i < 0.0 { u.len() as f64 + i } else { i };
    if i < 0.0 || i >= u.len() as f64 {
        return Ok(Value::UNDEFINED);
    }
    Ok(vm.single_char(u[i as usize]))
}

/// `normalize(form)`: the string itself (no normalization tables).
fn string_normalize(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let u = this_units(this)?;
    if let Some(f) = a.first().filter(|v| !v.is_undefined()) {
        let f = String::from_utf16_lossy(&units(*f));
        if !matches!(f.as_str(), "NFC" | "NFD" | "NFKC" | "NFKD") {
            return Err(Error::range_error("The normalization form should be one of NFC, NFD, NFKC, NFKD."));
        }
    }
    Ok(this_string(vm, this, &u))
}

/// `String.fromCodePoint(…points)`.
fn string_from_code_point(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let mut out = Vec::with_capacity(a.len());
    for &v in a {
        let d = to_number(v);
        if d.fract() != 0.0 || !(0.0..=0x10FFFF as f64).contains(&d) {
            return Err(Error::range_error(format!("Invalid code point {}", crate::vm::display(v))));
        }
        let cp = d as u32;
        if cp >= 0x10000 {
            let c = cp - 0x10000;
            out.push(0xD800 + (c >> 10) as u16);
            out.push(0xDC00 + (c & 0x3FF) as u16);
        } else {
            out.push(cp as u16);
        }
    }
    Ok(string(vm, &out))
}

/// Annex B's HTML methods: `"<tag attr=\"value\">this</tag>"`.
fn string_html<const K: usize>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    const TAGS: [(&str, &str); 13] = [
        ("a", "name"),
        ("big", ""),
        ("blink", ""),
        ("b", ""),
        ("tt", ""),
        ("font", "color"),
        ("font", "size"),
        ("i", ""),
        ("a", "href"),
        ("small", ""),
        ("strike", ""),
        ("sub", ""),
        ("sup", ""),
    ];
    let u = this_units(this)?;
    let (tag, attr) = TAGS[K];
    let mut s = format!("<{tag}");
    if !attr.is_empty() {
        let v = String::from_utf16_lossy(&units(arg(a, 0))).replace('"', "&quot;");
        s.push_str(&format!(" {attr}=\"{v}\""));
    }
    s.push('>');
    s.push_str(&String::from_utf16_lossy(&u));
    s.push_str(&format!("</{tag}>"));
    Ok(str_value(vm, &s))
}

/// `split(sep, limit)` with a string separator (or none: the whole
/// string; `""`: each code unit), or a regular expression (its captures
/// spliced in).
fn string_split(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if let Some(r) = delegate(vm, "split", this, a)? {
        return Ok(r);
    }
    let u = this_units(this)?.into_owned();
    if let Some(re) = regexp_of(arg(a, 0)) {
        return regexp_split(vm, re, &u, a.get(1).copied());
    }
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
    if string_units(this).is_some() {
        return Ok(this);
    }
    match wrapped(this) {
        Some(s) if string_units(s).is_some() => Ok(s),
        _ => Err(Error::type_error("String.prototype.toString called on a non-string")),
    }
}

// ---- Function.prototype.apply ----

/// The arguments `f.apply(that, list)` passes when `list` is a plain
/// array (holes as `undefined`; none for `undefined` or `null`), read
/// without running code: `None` for any other list, which
/// `Function.prototype.apply` itself takes (`array::apply_list`).
pub(crate) fn apply_list_fast(list: Value) -> Option<Vec<Value>> {
    if list.is_undefined() || list.is_null() {
        return Some(Vec::new());
    }
    if !list.is_cell() || unsafe { (*as_object(list)).class } != Class::Array {
        return None;
    }
    let o = as_object(list);
    unsafe {
        if (*o).flags & !(crate::object::EXTERNAL | crate::object::PROTOTYPE) != 0 {
            return None;
        }
    }
    let n = unsafe { (*o).elems_len } as usize;
    Some(
        (0..n)
            .map(|i| {
                let e = unsafe { *(*o).elems.add(i) };
                if e == Value::EMPTY { Value::UNDEFINED } else { e }
            })
            .collect(),
    )
}

/// `Function.prototype.apply(that, list)`. (A call through it to a
/// bytecode function with a plain array is unwrapped where it is made:
/// `interp::unwrap_apply`.)
fn function_apply(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::props::is_callable(this) {
        return Err(Error::type_error("Function.prototype.apply called on a non-function"));
    }
    vm.handle_scope(|vm| {
        let list = crate::array::apply_list(vm, arg(a, 1))?;
        crate::interp::call_function(vm, this, arg(a, 0), &list)
    })
}

/// `Function(p1, …, body)` / `new Function(…)`: the function
/// `(function anonymous(p1, …) { body })` evaluates to, in the global scope.
fn function_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    dynamic_function(vm, a, "function")
}

/// `GeneratorFunction(p1, …, body)`: as `Function`'s, a generator.
pub(crate) fn generator_function_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    dynamic_function(vm, a, "function*")
}

/// `AsyncFunction(p1, …, body)`: as `Function`'s, an async function.
pub(crate) fn async_function_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    dynamic_function(vm, a, "async function")
}

/// The function `(<kind> anonymous(p1, …) { body })` evaluates to.
fn dynamic_function(vm: &mut Vm, a: &[Value], kind: &str) -> Result<Value> {
    let mut params = Vec::new();
    for &p in a.iter().take(a.len().saturating_sub(1)) {
        params.push(String::from_utf16_lossy(&units(p)));
    }
    let body = a.last().map_or(String::new(), |&b| String::from_utf16_lossy(&units(b)));
    let src = format!("({kind} anonymous({}\n) {{\n{}\n}})", params.join(","), body);
    run_source(vm, &src)
}

/// `eval(x)`: `x` itself unless a string, else the value of the script it
/// is, run in the global scope (also when called directly: its `var`s and
/// functions become globals, and it can't see the caller's variables).
fn eval_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = arg(a, 0);
    let Some(u) = string_units(x) else {
        return Ok(x);
    };
    // (ASCII, as code mostly is: a byte per unit.)
    let src = if u.iter().all(|&c| c < 0x80) {
        String::from_utf8(u.iter().map(|&c| c as u8).collect()).unwrap()
    } else {
        String::from_utf16_lossy(u)
    };
    let module = crate::js::compile_indirect_eval(&src).map_err(|e| {
        if e.contains("unsupported") {
            Error(format!("unsupported: in eval'd code: {e}"), None)
        } else {
            Error(format!("SyntaxError: {e}"), None)
        }
    })?;
    vm.global_declarations(&module, true)?;
    let code = vm.load(module);
    crate::interp::run_script_at(vm, code)
}

/// `$262.evalScript(src)`: `src` run as a script of its own.
fn eval_script(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = arg(a, 0);
    let src = String::from_utf16_lossy(string_units(x).unwrap_or(&[]));
    let module = crate::js::compile_script_lazy(&src).map_err(|e| Error(format!("SyntaxError: {e}"), None))?;
    vm.declare_script(&module)?;
    let code = vm.load(module);
    crate::interp::run_script_at(vm, code)
}

/// `$262.gc()`: nothing (collections happen on their own).
fn host_gc(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::UNDEFINED)
}

/// `$262.detachArrayBuffer(buffer)`: the buffer loses its bytes (its
/// views see a detached buffer: `collections::view_bytes`).
fn host_detach_array_buffer(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let b = arg(a, 0);
    if !is_class(b, Class::ArrayBuffer) {
        return Err(Error::type_error("not an ArrayBuffer"));
    }
    unsafe {
        let o = as_object(b);
        (*o).elems_len = 0;
        (*o).flags |= crate::object::DETACHED;
    }
    Ok(Value::NULL)
}

/// `print(…)`: the arguments' strings, separated by spaces, on a line of
/// standard output.
fn print_fn(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let parts: Vec<String> = a.iter().map(|&v| String::from_utf16_lossy(&units(v))).collect();
    println!("{}", parts.join(" "));
    Ok(Value::UNDEFINED)
}

/// `String.raw(strings, …subs)`: `strings.raw`'s pieces, the substitutions
/// between them.
fn string_raw(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let cooked = object_value(vm.to_object(arg(a, 0))?);
    let a_raw = vm.intern("raw");
    let raw = vm.get(cooked, a_raw)?;
    let raw = object_value(vm.to_object(raw)?);
    let n = vm.length_of(raw)?;
    let mut out: Vec<u16> = Vec::new();
    let mut i = 0.0;
    while i < n {
        let piece = vm.get_keyed(raw, Value::number(i))?;
        let s = crate::intrinsics::to_string(vm, piece)?;
        out.extend_from_slice(string_units(s).unwrap_or(&[]));
        i += 1.0;
        if i < n && (i as usize) < a.len() {
            let s = crate::intrinsics::to_string(vm, a[i as usize])?;
            out.extend_from_slice(string_units(s).unwrap_or(&[]));
        }
    }
    Ok(vm.new_string(&out))
}

/// `read(file)`: reading files isn't supported.
fn read_fn(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(Error::unsupported("read()"))
}

/// Compile `src` as a script and run it (see `interp::run_script_at`): its
/// completion value. A syntax error is a `SyntaxError`.
pub(crate) fn run_source(vm: &mut Vm, src: &str) -> Result<Value> {
    let module = crate::js::compile_script_lazy(src).map_err(|e| {
        if e.contains("unsupported") {
            Error(format!("unsupported: in eval'd code: {e}"), None)
        } else {
            Error(format!("SyntaxError: {e}"), None)
        }
    })?;
    let code = vm.load(module);
    crate::interp::run_script_at(vm, code)
}

/// `f.bind(that, …args)`: a function calling `f` with `this` `that` and
/// `args` before its own. Made by a JS helper (natives carry no data of
/// their own), evaluated on first use.
fn function_bind(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !this.is_cell() || !matches!(unsafe { (*as_object(this)).class }, Class::Function | Class::Native) {
        return Err(Error::type_error("Bind must be called on a function"));
    }
    if vm.bind_helper.is_undefined() {
        // (Its calls through natives, not `apply` or `push`, which code may
        // have replaced.)
        let make = run_source(
            vm,
            "(function (call, construct) { return function (f, that, args, ctor) { \
               if (ctor) return function B(...rest) { \
                 return new.target ? construct(f, args, rest, new.target === B ? f : new.target) \
                   : call(f, that, args, rest); }; \
               return ({ bound(...rest) { return call(f, that, args, rest); } }).bound; }; })",
        )?;
        vm.bind_helper = vm.handle_scope(|vm| {
            vm.handles.push(make);
            let call = vm.native_function("call", bound_call, 4, crate::jit::Ty::Any, false);
            vm.handles.push(object_value(call));
            let construct = vm.native_function("construct", bound_construct, 4, crate::jit::Ty::Any, false);
            vm.handles.push(object_value(construct));
            crate::interp::call_function(vm, make, Value::UNDEFINED, &[object_value(call), object_value(construct)])
        })?;
    }
    let rest = vm.new_array();
    crate::vm::array_push(vm, rest, a.get(1..).unwrap_or(&[]))?;
    let helper = vm.bind_helper;
    // (A constructor only if `this` is one.)
    let ctor = Value::from_bool(crate::intrinsics::is_constructor(this));
    let bound = crate::interp::call_function(vm, helper, Value::UNDEFINED, &[this, arg(a, 0), rest, ctor])?;
    // (No `prototype` of its own.)
    let a_prototype = vm.a_prototype;
    vm.force_delete_named(as_object(bound), a_prototype);
    let a_target = vm.a_bound_target;
    vm.add_property_with(as_object(bound), a_target, this, DONT_ENUM | READ_ONLY | DONT_DELETE);
    // Its `length` (what the target's leaves) and `name` ("bound …").
    let a_length = vm.a_length;
    let len = if vm.own_property(as_object(this), crate::vm::Key::Name(a_length)).is_some() {
        let l = vm.get(this, a_length)?;
        match l.as_number() {
            Some(n) if n == f64::INFINITY => n,
            Some(n) if n.is_finite() => (n.trunc() - a.len().saturating_sub(1) as f64).max(0.0),
            Some(n) if n == f64::NEG_INFINITY => 0.0,
            _ => 0.0,
        }
    } else {
        0.0
    };
    let d = crate::props::Desc::data(Value::number(len), false, false, true);
    vm.define_own_property(as_object(bound), crate::vm::Key::Name(a_length), &d)?;
    let a_name = vm.a_name;
    let name = vm.get(this, a_name)?;
    let mut units: Vec<u16> = "bound ".encode_utf16().collect();
    if let Some(u) = string_units(name) {
        units.extend_from_slice(u);
    }
    let name = vm.new_string(&units);
    let d = crate::props::Desc::data(name, false, false, true);
    vm.define_own_property(as_object(bound), crate::vm::Key::Name(a_name), &d)?;
    Ok(bound)
}

/// A bound function's call: `[f, that, bound args, args]`.
fn bound_call(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    vm.handle_scope(|vm| {
        let mut all = crate::array::apply_list(vm, a[2])?;
        all.extend(crate::array::apply_list(vm, a[3])?);
        crate::interp::call_function(vm, a[0], a[1], &all)
    })
}

/// A bound function's construction: `[f, bound args, args, new.target]`.
fn bound_construct(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    vm.handle_scope(|vm| {
        let mut all = crate::array::apply_list(vm, a[1])?;
        all.extend(crate::array::apply_list(vm, a[2])?);
        crate::intrinsics::construct_with(vm, a[0], &all, a[3])
    })
}

/// `Function.prototype.toString`: a function's source text, or (a native,
/// a bound function) `function name() { [native code] }`.
fn function_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if !crate::props::is_callable(this) {
        return Err(Error::type_error("Function.prototype.toString requires that 'this' be a Function"));
    }
    let o = as_object(this);
    let code = unsafe { &*(*o).code };
    // (A bound function is the bind helper's closure.)
    let bound = vm.bind_helper.is_cell() && {
        let h = unsafe { &*(*as_object(vm.bind_helper)).code };
        h.closures.iter().any(|&c| std::ptr::eq(c, code))
    };
    if unsafe { (*o).class } == Class::Function
        && !bound
        && let Some((src, a, b)) = &code.text
        && let Some(t) = src.get(*a as usize..*b as usize)
    {
        let mut u = Vec::new();
        crate::js::lex::string_units(t, &mut u);
        return Ok(vm.new_string(&u));
    }
    let name = if bound { String::new() } else { code.name.clone() };
    Ok(str_value(vm, &format!("function {name}() {{ [native code] }}")))
}

/// A new array of `values`: an `arguments` object.
pub(crate) fn arguments_object(vm: &mut Vm, values: &[Value]) -> Value {
    arguments_object_of(vm, values, std::ptr::null_mut())
}

/// An `arguments` object of function `callee` (null: unknown, as strict
/// code's): an ordinary object with the values as elements, `length`,
/// `callee` (a strict function's throws), `@@iterator`, and the hidden
/// mark `Object.prototype.toString` knows it by.
pub(crate) fn arguments_object_of(vm: &mut Vm, values: &[Value], callee: *mut Object) -> Value {
    let strict = callee.is_null() || unsafe { (*(*callee).code).strict };
    if !strict {
        // The common case: one shape (its elements of any type from the
        // start, so `arguments.length` stays monomorphic), its slots and
        // elements stored.
        if vm.arguments_shape.is_null() {
            let op = object_value(vm.object_prototype);
            let mut s = vm.root_shape(Class::Plain, op, 4);
            let a_iter = vm.a_sym_iterator;
            let mark = vm.intern(ARGUMENTS_MARK);
            let a_callee = vm.intern("callee");
            for a in [vm.a_length, a_iter, mark, a_callee] {
                s = vm.transition_with(s, a, DONT_ENUM);
            }
            vm.arguments_shape = vm.elements_transition(s, crate::object::FieldType::Any);
        }
        let o = vm.heap.alloc_object(unsafe { &*vm.arguments_shape });
        let values_fn = vm.symbol_roots.get(crate::symbol::Root::ArrayValues as usize).copied();
        vm.store_field(o, 0, Value::from_i32(values.len() as i32));
        vm.store_field(o, 1, values_fn.map_or(Value::UNDEFINED, object_value));
        vm.store_field(o, 2, Value::TRUE);
        vm.store_field(o, 3, object_value(callee));
        vm.reserve_elems(o, values.len());
        for (i, &v) in values.iter().enumerate() {
            vm.store_element(o, i, v);
        }
        unsafe { (*o).elems_len = values.len() as u32 };
        return object_value(o);
    }
    let op = object_value(vm.object_prototype);
    let o = vm.new_object(op);
    let a_length = vm.a_length;
    vm.add_property_with(o, a_length, Value::from_i32(values.len() as i32), DONT_ENUM);
    let a_iter = vm.a_sym_iterator;
    let values_fn = vm.symbol_roots.get(crate::symbol::Root::ArrayValues as usize).copied();
    if let Some(f) = values_fn {
        vm.add_property_with(o, a_iter, object_value(f), DONT_ENUM);
    }
    let mark = vm.intern(ARGUMENTS_MARK);
    vm.add_property_with(o, mark, Value::TRUE, DONT_ENUM);
    let a_callee = vm.intern("callee");
    if strict {
        let thrower = vm.throw_type_error_fn();
        let d = crate::props::Desc {
            get: Some(thrower),
            set: Some(thrower),
            enumerable: Some(false),
            configurable: Some(false),
            ..Default::default()
        };
        let _ = vm.define_own_property(o, crate::vm::Key::Name(a_callee), &d);
    } else {
        vm.add_property_with(o, a_callee, object_value(callee), DONT_ENUM);
    }
    let ov = object_value(o);
    for (i, &v) in values.iter().enumerate() {
        let _ = vm.put_key(ov, crate::vm::Key::Index(i as u32), v);
    }
    ov
}

/// The hidden property marking `arguments` objects (a symbol atom no
/// symbol has: no enumeration lists it).
pub(crate) const ARGUMENTS_MARK: &str = "\u{FDD0}arguments";

// ---- Number and Object ----

fn np_of(vm: &Vm) -> *mut Object {
    vm.number_prototype
}

/// `Number(v)`: ToNumber (0 without an argument).
fn number_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let construct = vm.is_construct();
    let n = match a.first() {
        None => Value::from_i32(0),
        Some(&v) if v.is_number() => v,
        Some(&v) => num(to_number(v)),
    };
    Ok(if construct { vm.new_wrapper(n) } else { n })
}

fn boolean_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let construct = vm.is_construct();
    let b = Value::from_bool(crate::interp::to_boolean(arg(a, 0)));
    Ok(if construct { vm.new_wrapper(b) } else { b })
}

fn this_boolean(this: Value) -> Result<bool> {
    let b = wrapped(this).filter(|v| v.is_bool()).unwrap_or(this);
    if b.is_bool() {
        Ok(b == Value::TRUE)
    } else {
        Err(Error::type_error("Boolean.prototype method called on a non-boolean"))
    }
}

fn boolean_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let b = this_boolean(this)?;
    Ok(str_value(vm, if b { "true" } else { "false" }))
}

fn boolean_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(this_boolean(this)?))
}

/// `Object.create(proto)`: a plain object with prototype `proto` (an
/// object or `null`). (No property descriptors yet.)
fn number_to_exponential(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let n = wrapped(this).filter(|v| v.is_number()).unwrap_or(this);
    let Some(x) = n.as_number() else {
        return Err(Error::type_error("Number.prototype.toExponential called on a non-number"));
    };
    // (The digits converted first, then the value's finiteness.)
    let digits = match a.first() {
        Some(d) if !d.is_undefined() => {
            let f = to_number(*d);
            if vm.conv_thrown {
                return Err(vm.take_conv_error());
            }
            Some(if f.is_nan() { 0.0 } else { f.trunc() })
        }
        _ => None,
    };
    if !x.is_finite() {
        return Ok(str_value(vm, &number_to_string(x)));
    }
    let sign = if x < 0.0 { "-" } else { "" };
    let s = match digits {
        Some(f) => {
            if !(0.0..=100.0).contains(&f) {
                return Err(Error::range_error("toExponential() argument must be between 0 and 100"));
            }
            let f = f as usize;
            if x == 0.0 {
                if f == 0 { "0e0".to_string() } else { format!("0.{}e0", "0".repeat(f)) }
            } else {
                // `f + 1` digits, rounded half up.
                let (n, e) = crate::vm::significant(x.abs(), f + 1);
                let m = if f == 0 { n.clone() } else { format!("{}.{}", &n[..1], &n[1..]) };
                format!("{sign}{m}e{e}")
            }
        }
        None if x == 0.0 => "0e0".to_string(),
        None => format!("{x:e}"),
    };
    // Rust's `1.5e3` / `1.5e-3`: JavaScript's `1.5e+3` / `1.5e-3`.
    let s = match s.split_once('e') {
        Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
        _ => s,
    };
    Ok(str_value(vm, &s))
}

/// `Number.prototype.toLocaleString()`: as `toString()`.
fn number_to_locale_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let n = wrapped(this).filter(|v| v.is_number()).unwrap_or(this);
    let Some(x) = n.as_number() else {
        return Err(Error::type_error("Number.prototype.toLocaleString called on a non-number"));
    };
    Ok(str_value(vm, &number_to_string(x)))
}

// ---- What ToPrimitive calls ----

/// `Object.prototype.toString`: `[object Array]`, `[object Object]`, ….
fn object_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let primitive_tag = |v: Value| {
        if v.is_bool() {
            "Boolean"
        } else if v.is_number() {
            "Number"
        } else {
            "String"
        }
    };
    let tag = if this.is_undefined() {
        "Undefined"
    } else if this.is_null() {
        "Null"
    } else if this.is_cell() {
        let o = as_object(this);
        match unsafe { (*o).class } {
            Class::Array => "Array",
            Class::Function | Class::Native => "Function",
            Class::String => "String",
            Class::Date => "Date",
            Class::RegExp => "RegExp",
            Class::Wrapper => primitive_tag(wrapped(this).unwrap()),
            Class::Plain if unsafe { (*o).flags } & crate::object::ERROR != 0 => "Error",
            Class::Plain if {
                let mark = vm.intern(ARGUMENTS_MARK);
                vm.own_property(o, crate::vm::Key::Name(mark)).is_some()
            } =>
            {
                "Arguments"
            }
            Class::Map => "Map",
            Class::Set => "Set",
            Class::WeakMap => "WeakMap",
            Class::WeakSet => "WeakSet",
            Class::DataView => "DataView",
            Class::Promise => "Promise",
            Class::Symbol => "Symbol",
            // (A proxy of an array is one.)
            Class::Proxy if crate::proxy::is_array(vm, this)? => "Array",
            _ => "Object",
        }
    } else {
        primitive_tag(this)
    };
    // `@@toStringTag`, if a string.
    if this.is_cell()
        && let Some(t) = crate::symbol::tag_of(vm, this)
    {
        let mut u: Vec<u16> = "[object ".encode_utf16().collect();
        u.extend_from_slice(&t);
        u.push(b']' as u16);
        return Ok(vm.new_string(&u));
    }
    Ok(str_value(vm, &format!("[object {tag}]")))
}

fn object_value_of(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(object_value(vm.to_object(this)?))
}

fn number_value_of(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let n = wrapped(this).filter(|v| v.is_number()).unwrap_or(this);
    if n.is_number() {
        Ok(n)
    } else {
        Err(Error::type_error("Number.prototype.valueOf called on a non-number"))
    }
}

// ---- Math ----

fn math_arg(a: &[Value], i: usize) -> f64 {
    a.get(i).map_or(f64::NAN, |&v| to_number(v))
}
fn math_tan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(crate::math::tan(math_arg(a, 0))))
}
fn math_atan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(crate::math::atan(math_arg(a, 0))))
}
fn math_atan2(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(crate::math::atan2(math_arg(a, 0), math_arg(a, 1))))
}
fn math_asin(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(crate::math::asin(math_arg(a, 0))))
}
fn math_acos(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(crate::math::acos(math_arg(a, 0))))
}
fn math_cbrt(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).cbrt()))
}
fn math_clz32(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_i32((crate::interp::to_int32(math_arg(a, 0)) as u32).leading_zeros() as i32))
}
fn math_cosh(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).cosh()))
}
fn math_sinh(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).sinh()))
}
fn math_tanh(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).tanh()))
}
fn math_acosh(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).acosh()))
}
fn math_asinh(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = math_arg(a, 0);
    Ok(num(if x == 0.0 { x } else { x.asinh() }))
}
fn math_atanh(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = math_arg(a, 0);
    Ok(num(if x == 0.0 { x } else { x.atanh() }))
}
fn math_expm1(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = math_arg(a, 0);
    Ok(num(if x == 0.0 { x } else { x.exp_m1() }))
}
fn math_log1p(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = math_arg(a, 0);
    Ok(num(if x == 0.0 { x } else { x.ln_1p() }))
}
fn math_log2(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).log2()))
}
fn math_log10(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).log10()))
}
fn math_sign(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let x = math_arg(a, 0);
    Ok(num(if x.is_nan() || x == 0.0 { x } else { x.signum() }))
}
fn math_trunc(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0).trunc()))
}
fn math_fround(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(num(math_arg(a, 0) as f32 as f64))
}
/// `Math.hypot(…values)`: every argument coerced first; infinity wins
/// over NaN.
fn math_hypot(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let mut xs: Vec<f64> = Vec::with_capacity(a.len());
    for i in 0..a.len() {
        xs.push(math_arg(a, i));
        // (No more conversions after one throws.)
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
    }
    if xs.iter().any(|x| x.is_infinite()) {
        return Ok(num(f64::INFINITY));
    }
    if xs.iter().any(|x| x.is_nan()) {
        return Ok(num(f64::NAN));
    }
    let max = xs.iter().fold(0.0f64, |m, &x| m.max(x.abs()));
    if max == 0.0 {
        return Ok(num(0.0));
    }
    let sum: f64 = xs.iter().map(|&x| (x / max) * (x / max)).sum();
    Ok(num(sum.sqrt() * max))
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
    // (Its string: -0's is "0".)
    if v.is_number() {
        return Ok(if v.as_number() == Some(0.0) { Value::from_i32(0) } else { v });
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

// ---- escape, unescape, URIs ----

const HEX: &[u8; 16] = b"0123456789ABCDEF";

fn escape_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let u = units(arg(a, 0)).into_owned();
    let mut out = Vec::new();
    for c in u {
        let keep = c < 128 && (c as u8).is_ascii_alphanumeric() || b"@*_+-./".contains(&(c as u8)) && c < 128;
        if keep {
            out.push(c);
        } else if c < 256 {
            out.extend([b'%' as u16, HEX[(c >> 4) as usize] as u16, HEX[(c & 15) as usize] as u16]);
        } else {
            out.extend(['%', 'u'].iter().map(|&c| c as u16));
            for k in (0..4).rev() {
                out.push(HEX[((c >> (4 * k)) & 15) as usize] as u16);
            }
        }
    }
    Ok(vm.new_string(&out))
}

fn hex_digits(u: &[u16]) -> Option<u16> {
    let s = String::from_utf16(u).ok()?;
    u16::from_str_radix(&s, 16).ok().filter(|_| s.chars().all(|c| c.is_ascii_hexdigit()))
}

fn unescape_fn(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let u = units(arg(a, 0)).into_owned();
    let mut out = Vec::new();
    let mut i = 0;
    while i < u.len() {
        if u[i] == b'%' as u16 {
            if u.get(i + 1) == Some(&(b'u' as u16))
                && let Some(c) = u.get(i + 2..i + 6).and_then(hex_digits)
            {
                out.push(c);
                i += 6;
                continue;
            }
            if let Some(c) = u.get(i + 1..i + 3).and_then(hex_digits) {
                out.push(c);
                i += 3;
                continue;
            }
        }
        out.push(u[i]);
        i += 1;
    }
    Ok(vm.new_string(&out))
}

/// `encodeURI…`: UTF-8 bytes as `%XX`, but for `keep`'s characters.
fn encode(vm: &mut Vm, a: &[Value], keep: &[u8]) -> Result<Value> {
    let u = units(arg(a, 0)).into_owned();
    let mut out = Vec::new();
    let mut it = char::decode_utf16(u.iter().copied());
    while let Some(c) = it.next() {
        let c = c.map_err(|_| Error(String::from("URIError: URI malformed"), None))?;
        if c.is_ascii_alphanumeric() || (c.is_ascii() && keep.contains(&(c as u8))) {
            out.push(c as u16);
            continue;
        }
        let mut b = [0u8; 4];
        for &byte in c.encode_utf8(&mut b).as_bytes() {
            out.extend([b'%' as u16, HEX[(byte >> 4) as usize] as u16, HEX[(byte & 15) as usize] as u16]);
        }
    }
    Ok(vm.new_string(&out))
}

fn encode_uri_component(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    encode(vm, a, b"-_.!~*'()")
}

fn encode_uri(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    encode(vm, a, b"-_.!~*'();/?:@&=+$,#")
}

/// `decodeURI…`: `%XX` UTF-8 sequences decoded, but those of `reserved`'s
/// characters.
fn decode(vm: &mut Vm, a: &[Value], reserved: &[u8]) -> Result<Value> {
    let malformed = || Error(String::from("URIError: URI malformed"), None);
    let u = units(arg(a, 0)).into_owned();
    let mut out = Vec::new();
    let mut i = 0;
    while i < u.len() {
        if u[i] != b'%' as u16 {
            out.push(u[i]);
            i += 1;
            continue;
        }
        let byte = |i: usize| u.get(i + 1..i + 3).and_then(hex_digits).map(|b| b as u8);
        let b0 = byte(i).ok_or_else(malformed)?;
        if b0 < 0x80 {
            if reserved.contains(&b0) {
                out.extend_from_slice(&u[i..i + 3]);
            } else {
                out.push(b0 as u16);
            }
            i += 3;
            continue;
        }
        let n = match b0 {
            0xC0..=0xDF => 2,
            0xE0..=0xEF => 3,
            0xF0..=0xF7 => 4,
            _ => return Err(malformed()),
        };
        let mut bytes = vec![b0];
        for k in 1..n {
            if u.get(i + 3 * k) != Some(&(b'%' as u16)) {
                return Err(malformed());
            }
            bytes.push(byte(i + 3 * k).ok_or_else(malformed)?);
        }
        let s = std::str::from_utf8(&bytes).map_err(|_| malformed())?;
        out.extend(s.encode_utf16());
        i += 3 * n;
    }
    Ok(vm.new_string(&out))
}

fn decode_uri_component(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    decode(vm, a, b"")
}

fn decode_uri(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    decode(vm, a, b";/?:@&=+$,#")
}

/// `%ThrowTypeError%`.
fn throw_type_error(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(Error::type_error("'caller', 'callee', and 'arguments' properties may not be accessed on strict mode functions or the arguments objects for calls to them"))
}

fn number_is_integer(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    Ok(Value::from_bool(v.as_number().is_some_and(|d| d.is_finite() && d.trunc() == d)))
}

fn number_is_safe_integer(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let v = arg(a, 0);
    Ok(Value::from_bool(v.as_number().is_some_and(|d| d.is_finite() && d.trunc() == d && d.abs() <= 9007199254740991.0)))
}

fn number_is_finite(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(arg(a, 0).as_number().is_some_and(f64::is_finite)))
}

fn number_is_nan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(arg(a, 0).as_number().is_some_and(f64::is_nan)))
}

fn is_nan(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(math_arg(a, 0).is_nan()))
}

fn is_finite(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(math_arg(a, 0).is_finite()))
}

// ---- Typed arrays ----

/// `ArrayBuffer`, and the typed array constructors, whose prototypes
/// share one holding their methods (as `%TypedArray%.prototype`).
fn install_typed_arrays(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = Ty::Any;
    let abp = vm.new_object(op);
    vm.array_buffer_prototype = abp;
    let ab = vm.define_native(g, "ArrayBuffer", array_buffer_ctor, 1, any, true);
    link_constructor(vm, ab, abp);
    getter(vm, abp, "byteLength", buffer_byte_length);
    vm.define_native(abp, "slice", buffer_slice, 2, any, false);
    vm.define_native(ab, "isView", buffer_is_view, 1, any, false);

    // `%TypedArray%` (abstract) and its prototype, the concrete kinds'.
    let shared = vm.new_object(op);
    let ta = vm.native_function("TypedArray", typed_array_abstract, 0, any, true);
    link_constructor(vm, ta, shared);
    vm.define_natives(ta, &[("from", typed_from as NativeFn, 1), ("of", typed_of, 0)]);
    vm.define_natives(
        shared,
        &[
            ("subarray", typed_subarray as NativeFn, 2),
            ("set", typed_set, 2),
            ("join", typed_join, 1),
            ("toString", typed_to_string, 0),
            ("fill", typed_fill, 3),
            ("map", typed_map, 1),
            ("filter", typed_filter, 1),
            ("slice", typed_slice, 2),
            ("sort", typed_sort, 1),
            ("toSorted", typed_to_sorted, 1),
            ("toReversed", typed_to_reversed, 0),
            ("with", typed_with, 2),
            ("at", typed_generic::<0>, 1),
            ("copyWithin", typed_generic::<1>, 2),
            ("every", typed_generic::<2>, 1),
            ("find", typed_generic::<3>, 1),
            ("findIndex", typed_generic::<4>, 1),
            ("findLast", typed_generic::<5>, 1),
            ("findLastIndex", typed_generic::<6>, 1),
            ("forEach", typed_generic::<7>, 1),
            ("includes", typed_generic::<8>, 1),
            ("indexOf", typed_generic::<9>, 1),
            ("lastIndexOf", typed_generic::<10>, 1),
            ("reduce", typed_generic::<11>, 1),
            ("reduceRight", typed_generic::<12>, 1),
            ("reverse", typed_generic::<13>, 0),
            ("some", typed_generic::<14>, 1),
            ("toLocaleString", typed_generic::<15>, 0),
        ],
    );
    getter(vm, shared, "buffer", typed_buffer);
    getter(vm, shared, "byteLength", typed_byte_length);
    getter(vm, shared, "byteOffset", typed_byte_offset);
    const CTORS: [NativeFn; 9] = [
        typed_ctor::<0>,
        typed_ctor::<1>,
        typed_ctor::<2>,
        typed_ctor::<3>,
        typed_ctor::<4>,
        typed_ctor::<5>,
        typed_ctor::<6>,
        typed_ctor::<7>,
        typed_ctor::<8>,
    ];
    let a_bpe = vm.intern("BYTES_PER_ELEMENT");
    for (k, kind) in TypedKind::ALL.iter().enumerate() {
        let proto = vm.new_object(object_value(shared));
        let c = vm.define_native(g, kind.name(), CTORS[k], 3, any, true);
        link_constructor(vm, c, proto);
        vm.set_prototype_of(c, object_value(ta));
        let size = Value::from_i32(kind.size() as i32);
        vm.add_property_with(c, a_bpe, size, READ_ONLY | DONT_ENUM | DONT_DELETE);
        vm.add_property_with(proto, a_bpe, size, READ_ONLY | DONT_ENUM | DONT_DELETE);
        vm.typed_prototypes.push(proto);
    }
}

/// An accessor `name` on `o` whose getter is the native `f` (named `get
/// name`, as the spec has it).
pub(crate) fn getter(vm: &mut Vm, o: *mut Object, name: &str, f: NativeFn) {
    let holder = vm.new_object(Value::NULL);
    let fo = vm.define_native(holder, &format!("get {name}"), f, 0, Ty::Any, false);
    let a = vm.intern(name);
    vm.define_accessor(o, a, Some(object_value(fo)), None, DONT_ENUM);
}

/// `ArrayBuffer.isView(v)`: a typed array or a `DataView`.
fn buffer_is_view(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(is_class(arg(a, 0), Class::TypedArray)))
}

/// A new buffer of `n` zero bytes.
pub(crate) fn new_buffer(vm: &mut Vm, n: usize) -> *mut Object {
    let abp = object_value(vm.array_buffer_prototype);
    let o = vm.new_object_with(Class::ArrayBuffer, abp, 0);
    unsafe {
        (*o).elems = vm.heap.alloc_bytes(8 * n.div_ceil(8).max(1)).cast();
        (*o).elems_len = u32::try_from(n).expect("buffer too large");
    }
    o
}

/// A view of kind `k` on `buffer`'s bytes from `offset`, `len` elements.
fn new_view(vm: &mut Vm, k: TypedKind, buffer: *mut Object, offset: usize, len: usize) -> Value {
    let proto = vm.typed_prototypes[k as usize];
    let o = vm.new_object_with(Class::TypedArray, object_value(proto), 0);
    unsafe {
        (*o).kind = k as u8;
        (*o).elems = (*buffer).elems.cast::<u8>().add(offset).cast();
        (*o).elems_len = len as u32;
        (*o).code = buffer.cast();
    }
    object_value(o)
}

pub(crate) fn is_class(v: Value, c: Class) -> bool {
    v.is_cell() && unsafe { (*as_object(v)).class } == c
}

/// ToIndex of argument `i` (`default` if missing or undefined).
fn index_arg(a: &[Value], i: usize, default: usize) -> Result<usize> {
    match a.get(i) {
        Some(v) if !v.is_undefined() => {
            let d = to_number(*v);
            let d = if d.is_nan() { 0.0 } else { d.trunc() };
            if !(0.0..=9007199254740991.0).contains(&d) {
                return Err(Error::range_error("Invalid typed array length"));
            }
            Ok(d as usize)
        }
        _ => Ok(default),
    }
}

fn array_buffer_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor ArrayBuffer requires 'new'"));
    }
    let n = index_arg(a, 0, 0)?;
    if n > 1 << 31 {
        return Err(Error::range_error("Array buffer allocation failed"));
    }
    Ok(object_value(new_buffer(vm, n)))
}

fn this_buffer(this: Value) -> Result<*mut Object> {
    if is_class(this, Class::ArrayBuffer) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error("not an ArrayBuffer"))
    }
}

fn buffer_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_i32(unsafe { (*this_buffer(this)?).elems_len } as i32))
}

/// `buffer.slice(begin, end)`: a copy of those bytes.
fn buffer_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let b = this_buffer(this)?;
    let len = unsafe { (*b).elems_len } as usize;
    let s = relative(int_arg(a, 0), len);
    let e = match a.get(1) {
        Some(v) if !v.is_undefined() => relative(int_arg(a, 1), len),
        _ => len,
    };
    let n = e.saturating_sub(s);
    let c = new_buffer(vm, n);
    unsafe { std::ptr::copy_nonoverlapping((*b).elems.cast::<u8>().add(s), (*c).elems.cast::<u8>(), n) };
    Ok(object_value(c))
}

/// `new X(length)`, `new X(buffer, byteOffset, length)`, or `new X(list)`
/// (an array, typed array or array-like: its elements converted).
fn typed_ctor<const K: usize>(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let k = TypedKind::ALL[K];
    let first = arg(a, 0);
    if is_class(first, Class::ArrayBuffer) {
        let b = as_object(first);
        let bytes = unsafe { (*b).elems_len } as usize;
        let offset = index_arg(a, 1, 0)?;
        if offset % k.size() != 0 {
            return Err(Error::range_error(format!(
                "start offset of {} should be a multiple of {}",
                k.name(),
                k.size()
            )));
        }
        let len = match a.get(2) {
            Some(v) if !v.is_undefined() => index_arg(a, 2, 0)?,
            _ => {
                if bytes % k.size() != 0 || offset > bytes {
                    return Err(Error::range_error("Invalid typed array length"));
                }
                (bytes - offset) / k.size()
            }
        };
        if offset + len * k.size() > bytes {
            return Err(Error::range_error("Invalid typed array length"));
        }
        return Ok(new_view(vm, k, b, offset, len));
    }
    if first.is_cell() && string_units(first).is_none() {
        // A list: its length, then each element.
        let a_length = vm.intern("length");
        let n = to_number(vm.get(first, a_length)?);
        let n = if n.is_nan() || n < 0.0 { 0 } else { n as usize };
        typed_length(n, k.size())?;
        let b = new_buffer(vm, n * k.size());
        let v = new_view(vm, k, b, 0, n);
        for i in 0..n {
            let e = vm.get_keyed(first, Value::from_i32(i as i32))?;
            unsafe { k.store((*b).elems.cast(), i, to_number(e)) };
        }
        return Ok(v);
    }
    let n = index_arg(a, 0, 0)?;
    typed_length(n, k.size())?;
    let b = new_buffer(vm, n * k.size());
    Ok(new_view(vm, k, b, 0, n))
}

/// A typed array of `n` elements `size` bytes each fits a buffer (as
/// `ArrayBuffer`'s limit), else a RangeError.
fn typed_length(n: usize, size: usize) -> Result<()> {
    if n.checked_mul(size).is_none_or(|b| b > 1 << 31) {
        return Err(Error::range_error(format!("Invalid typed array length: {n}")));
    }
    Ok(())
}

fn this_typed(this: Value) -> Result<*mut Object> {
    if is_class(this, Class::TypedArray) {
        Ok(as_object(this))
    } else {
        Err(Error::type_error("this is not a typed array."))
    }
}

fn kind_of(o: *mut Object) -> TypedKind {
    TypedKind::from_u8(unsafe { (*o).kind })
}

/// `t.subarray(begin, end)`: a view of the same bytes.
fn typed_subarray(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let k = kind_of(o);
    let len = unsafe { (*o).elems_len } as usize;
    let s = relative(int_arg(a, 0), len);
    let e = match a.get(1) {
        Some(v) if !v.is_undefined() => relative(int_arg(a, 1), len),
        _ => len,
    };
    unsafe {
        let b = (*o).code as *mut Object;
        let offset = (*o).elems.cast::<u8>().offset_from((*b).elems.cast::<u8>()) as usize;
        Ok(new_view(vm, k, b, offset + s * k.size(), e.saturating_sub(s)))
    }
}

/// `t.set(list, offset)`: `list`'s elements (an array or a typed array)
/// stored from `offset` on.
fn typed_set(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let k = kind_of(o);
    let offset = index_arg(a, 1, 0)?;
    let src = arg(a, 0);
    let a_length = vm.intern("length");
    let n = to_number(vm.get(src, a_length)?);
    let n = if n.is_nan() || n < 0.0 { 0 } else { n as usize };
    let len = unsafe { (*o).elems_len } as usize;
    if offset + n > len {
        return Err(Error::range_error("offset is out of bounds"));
    }
    // Read them all first: `list` may share the bytes.
    let mut vals = Vec::with_capacity(n);
    for i in 0..n {
        vals.push(to_number(vm.get_keyed(src, Value::from_i32(i as i32))?));
    }
    for (i, d) in vals.into_iter().enumerate() {
        unsafe { k.store((*o).elems.cast(), offset + i, d) };
    }
    Ok(Value::UNDEFINED)
}

/// `t.fill(v, begin, end)`.
fn typed_fill(_: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let k = kind_of(o);
    let len = unsafe { (*o).elems_len } as usize;
    let d = to_number(arg(a, 0));
    let s = relative(int_arg(a, 1), len);
    let e = match a.get(2) {
        Some(v) if !v.is_undefined() => relative(int_arg(a, 2), len),
        _ => len,
    };
    for i in s..e.max(s) {
        unsafe { k.store((*o).elems.cast(), i, d) };
    }
    Ok(this)
}

fn typed_join(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let k = kind_of(o);
    let sep = match a.first() {
        Some(v) if !v.is_undefined() => units(*v).into_owned(),
        _ => vec![b',' as u16],
    };
    let mut out = Vec::new();
    for i in 0..unsafe { (*o).elems_len } as usize {
        if i > 0 {
            out.extend_from_slice(&sep);
        }
        let v = unsafe { k.load((*o).elems.cast(), i) };
        out.extend(crate::vm::display(v).encode_utf16());
    }
    Ok(vm.new_string(&out))
}

fn typed_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    typed_join(vm, this, &[])
}

/// `%TypedArray%` itself: not to be called or constructed.
fn typed_array_abstract(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(Error::type_error("Abstract class TypedArray not directly constructable"))
}

/// The Array methods a typed array's of the index delegates to, once
/// `this` is checked to be one.
const TYPED_GENERIC: [NativeFn; 16] = [
    crate::array::array_at,
    crate::array::array_copy_within,
    crate::array::array_every,
    crate::array::array_find,
    crate::array::array_find_index,
    crate::array::array_find_last,
    crate::array::array_find_last_index,
    crate::array::array_for_each,
    crate::array::array_includes,
    crate::array::array_index_of,
    crate::array::array_last_index_of,
    crate::array::array_reduce,
    crate::array::array_reduce_right,
    crate::array::array_reverse,
    crate::array::array_some,
    crate::array::array_to_locale_string,
];

fn typed_generic<const K: usize>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    this_typed(this)?;
    TYPED_GENERIC[K](vm, this, a)
}

/// `get %TypedArray%.prototype[Symbol.toStringTag]`: the kind's name.
pub(crate) fn typed_to_string_tag(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if !is_class(this, Class::TypedArray) {
        return Ok(Value::UNDEFINED);
    }
    Ok(str_value(vm, kind_of(as_object(this)).name()))
}

/// A new typed array of kind `k`, `n` elements of `values` (zero past
/// them).
fn new_typed(vm: &mut Vm, k: TypedKind, n: usize, values: &[f64]) -> Result<Value> {
    typed_length(n, k.size())?;
    let b = new_buffer(vm, n * k.size());
    let v = new_view(vm, k, b, 0, n);
    for (i, &d) in values.iter().enumerate() {
        unsafe { k.store((*b).elems.cast(), i, d) };
    }
    Ok(v)
}

/// Typed array `o`'s elements as numbers.
fn typed_values(o: *mut Object) -> Vec<f64> {
    let k = kind_of(o);
    (0..unsafe { (*o).elems_len } as usize)
        .map(|i| unsafe { k.load((*o).elems.cast(), i) }.to_number())
        .collect()
}

/// The callback argument, checked callable.
fn typed_callback(a: &[Value], what: &str) -> Result<Value> {
    let f = arg(a, 0);
    if !crate::props::is_callable(f) {
        return Err(Error::type_error(format!("{} is not a function (%TypedArray%.prototype.{what})", crate::vm::display(f))));
    }
    Ok(f)
}

/// `t.map(f, thisArg)`: a typed array of the kind of `f`'s results.
fn typed_map(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let f = typed_callback(a, "map")?;
    let k = kind_of(o);
    let n = unsafe { (*o).elems_len } as usize;
    let out = new_typed(vm, k, n, &[])?;
    for i in 0..n {
        let v = if i < unsafe { (*o).elems_len } as usize { unsafe { k.load((*o).elems.cast(), i) } } else { Value::UNDEFINED };
        let r = crate::interp::call_function(vm, f, arg(a, 1), &[v, Value::from_i32(i as i32), this])?;
        vm.set_keyed(out, Value::from_i32(i as i32), r)?;
    }
    Ok(out)
}

/// `t.filter(f, thisArg)`: a typed array of the kind of the elements `f`
/// accepts.
fn typed_filter(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let f = typed_callback(a, "filter")?;
    let k = kind_of(o);
    let mut kept = Vec::new();
    for i in 0..unsafe { (*o).elems_len } as usize {
        let v = unsafe { k.load((*o).elems.cast(), i) };
        let r = crate::interp::call_function(vm, f, arg(a, 1), &[v, Value::from_i32(i as i32), this])?;
        if crate::interp::to_boolean(r) {
            kept.push(v.to_number());
        }
    }
    new_typed(vm, k, kept.len(), &kept)
}

/// `t.slice(begin, end)`: a copy of those elements.
fn typed_slice(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let len = unsafe { (*o).elems_len } as usize;
    let s = relative(int_arg(a, 0), len);
    let e = match a.get(1) {
        Some(v) if !v.is_undefined() => relative(int_arg(a, 1), len),
        _ => len,
    };
    let values = typed_values(o);
    let part = &values[s.min(len)..e.max(s).min(len)];
    new_typed(vm, kind_of(o), part.len(), part)
}

/// Sort `values` (stably) by `cmp` (a function, or `undefined`: numeric
/// order, -0 before +0, NaN last).
fn sort_numbers(vm: &mut Vm, values: &mut Vec<f64>, cmp: Value) -> Result<()> {
    if !cmp.is_undefined() && !crate::props::is_callable(cmp) {
        return Err(Error::type_error("The comparison function must be either a function or undefined"));
    }
    let after = |vm: &mut Vm, x: f64, y: f64| -> Result<bool> {
        if cmp.is_undefined() {
            return Ok(match (x.is_nan(), y.is_nan()) {
                (true, _) => !y.is_nan(),
                (false, true) => false,
                _ => x > y || (x == 0.0 && y == 0.0 && x.is_sign_positive() && y.is_sign_negative()),
            });
        }
        let r = crate::interp::call_function(vm, cmp, Value::UNDEFINED, &[Value::number(x), Value::number(y)])?;
        Ok(to_number(r) > 0.0)
    };
    // A merge sort: a comparator that isn't consistent still ends.
    let mut width = 1;
    let n = values.len();
    let mut buf = values.clone();
    while width < n {
        let mut lo = 0;
        while lo < n {
            let mid = (lo + width).min(n);
            let hi = (lo + 2 * width).min(n);
            let (mut i, mut j, mut k) = (lo, mid, lo);
            while i < mid && j < hi {
                if after(vm, values[i], values[j])? {
                    buf[k] = values[j];
                    j += 1;
                } else {
                    buf[k] = values[i];
                    i += 1;
                }
                k += 1;
            }
            buf[k..k + mid - i].copy_from_slice(&values[i..mid]);
            k += mid - i;
            buf[k..k + hi - j].copy_from_slice(&values[j..hi]);
            lo = hi;
        }
        std::mem::swap(values, &mut buf);
        width *= 2;
    }
    Ok(())
}

/// `t.sort(cmp)`: in place.
fn typed_sort(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let cmp = arg(a, 0);
    if !cmp.is_undefined() && !crate::props::is_callable(cmp) {
        return Err(Error::type_error("The comparison function must be either a function or undefined"));
    }
    let o = this_typed(this)?;
    let mut values = typed_values(o);
    sort_numbers(vm, &mut values, cmp)?;
    let k = kind_of(o);
    for (i, d) in values.into_iter().enumerate() {
        if i < unsafe { (*o).elems_len } as usize {
            unsafe { k.store((*o).elems.cast(), i, d) };
        }
    }
    Ok(this)
}

/// `t.toSorted(cmp)`: a sorted copy.
fn typed_to_sorted(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let cmp = arg(a, 0);
    if !cmp.is_undefined() && !crate::props::is_callable(cmp) {
        return Err(Error::type_error("The comparison function must be either a function or undefined"));
    }
    let o = this_typed(this)?;
    let mut values = typed_values(o);
    sort_numbers(vm, &mut values, cmp)?;
    new_typed(vm, kind_of(o), values.len(), &values)
}

/// `t.toReversed()`: a reversed copy.
fn typed_to_reversed(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let mut values = typed_values(o);
    values.reverse();
    new_typed(vm, kind_of(o), values.len(), &values)
}

/// `t.with(i, v)`: a copy with element `i` (from the end if negative) `v`.
fn typed_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    let len = unsafe { (*o).elems_len } as f64;
    let n = to_number(arg(a, 0));
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let i = if n < 0.0 { len + n } else { n };
    let d = to_number(arg(a, 1));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    if i < 0.0 || i >= unsafe { (*o).elems_len } as f64 {
        return Err(Error::range_error("Invalid typed array index"));
    }
    let mut values = typed_values(o);
    values[i as usize] = d;
    new_typed(vm, kind_of(o), values.len(), &values)
}

/// `TA.from(source, mapFn, thisArg)`: a new `this` typed array of the
/// source's values (iterated, or of an array-like), mapped.
fn typed_from(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::intrinsics::is_constructor(this) {
        return Err(Error::type_error("TypedArray.from: this is not a constructor"));
    }
    let (source, f) = (arg(a, 0), arg(a, 1));
    if !f.is_undefined() && !crate::props::is_callable(f) {
        return Err(Error::type_error("TypedArray.from: the mapping function is not callable"));
    }
    let a_iter = vm.a_sym_iterator;
    let iter = if crate::props::is_object(source) || string_units(source).is_some() {
        let o = object_value(vm.to_object(source)?);
        vm.get(o, a_iter)?
    } else {
        Value::UNDEFINED
    };
    let values = vm.handle_scope(|vm| -> Result<Vec<Value>> {
        let mut values = Vec::new();
        if !iter.is_undefined() && !iter.is_null() {
            crate::symbol::iterate(vm, source, |vm, v| {
                vm.handles.push(v);
                values.push(v);
                Ok(())
            })?;
        } else {
            let o = object_value(vm.to_object(source)?);
            let n = vm.length_of(o)?;
            let mut i = 0.0;
            while i < n {
                let v = vm.get_keyed(o, Value::number(i))?;
                vm.handles.push(v);
                values.push(v);
                i += 1.0;
            }
        }
        Ok(values)
    })?;
    let out = crate::intrinsics::construct(vm, this, &[Value::from_i32(values.len() as i32)])?;
    if !is_class(out, Class::TypedArray) || (unsafe { (*as_object(out)).elems_len } as usize) < values.len() {
        return Err(Error::type_error("TypedArray.from: the constructor made no typed array that long"));
    }
    for (i, &v) in values.iter().enumerate() {
        let v = if f.is_undefined() {
            v
        } else {
            crate::interp::call_function(vm, f, arg(a, 2), &[v, Value::from_i32(i as i32)])?
        };
        vm.set_keyed(out, Value::from_i32(i as i32), v)?;
    }
    Ok(out)
}

/// `TA.of(...items)`: a new `this` typed array of them.
fn typed_of(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::intrinsics::is_constructor(this) {
        return Err(Error::type_error("TypedArray.of: this is not a constructor"));
    }
    let out = crate::intrinsics::construct(vm, this, &[Value::from_i32(a.len() as i32)])?;
    if !is_class(out, Class::TypedArray) || (unsafe { (*as_object(out)).elems_len } as usize) < a.len() {
        return Err(Error::type_error("TypedArray.of: the constructor made no typed array that long"));
    }
    for (i, &v) in a.iter().enumerate() {
        vm.set_keyed(out, Value::from_i32(i as i32), v)?;
    }
    Ok(out)
}

fn typed_buffer(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(object_value(unsafe { (*this_typed(this)?).code } as *mut Object))
}

fn typed_byte_length(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    Ok(Value::from_i32((unsafe { (*o).elems_len } as usize * kind_of(o).size()) as i32))
}

fn typed_byte_offset(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = this_typed(this)?;
    unsafe {
        let b = (*o).code as *mut Object;
        Ok(Value::from_i32((*o).elems.cast::<u8>().offset_from((*b).elems.cast::<u8>()) as i32))
    }
}

// ---- JSON ----

// ---- Regular expressions ----

fn install_regexps(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = Ty::Any;
    let rp = vm.new_object(op);
    vm.regexp_prototype = rp;
    let c = vm.define_native(g, "RegExp", regexp_ctor, 2, any, true);
    link_constructor(vm, c, rp);
    vm.define_natives(
        rp,
        &[
            ("exec", regexp_exec as NativeFn, 1),
            ("test", regexp_test, 1),
            ("toString", regexp_to_string, 0),
        ],
    );
    vm.regexp_atoms[0] = vm.intern("exec");
    vm.regexp_fns[0] = as_object(vm.get(object_value(rp), vm.regexp_atoms[0]).unwrap());
    // `[Symbol.match]` and the others: the String methods', the RegExp
    // the pattern.
    let symbols: [(&str, NativeFn, u32); 5] = [
        ("match", regexp_symbol::<0>, 1),
        ("matchAll", regexp_symbol::<1>, 1),
        ("replace", regexp_symbol::<2>, 2),
        ("search", regexp_symbol::<3>, 1),
        ("split", regexp_symbol::<4>, 2),
    ];
    for (k, (name, f, n)) in symbols.into_iter().enumerate() {
        let fo = vm.native_function(&format!("[Symbol.{name}]"), f, n, Ty::Any, false);
        let a = vm.intern(&format!("{}Symbol.{name}", crate::symbol::SYMBOL_MARK));
        vm.add_property_with(rp, a, object_value(fo), DONT_ENUM);
        vm.regexp_fns[1 + k] = fo;
        vm.regexp_atoms[1 + k] = a;
    }
    // The flags and the source are accessors of the prototype (an
    // instance's own data properties shadow them: `regexp_object`).
    vm.define_native(rp, "compile", regexp_compile, 2, Ty::Any, false);
    getter(vm, rp, "source", regexp_get::<0>);
    getter(vm, rp, "flags", regexp_get::<1>);
    getter(vm, rp, "global", regexp_get::<2>);
    getter(vm, rp, "ignoreCase", regexp_get::<3>);
    getter(vm, rp, "multiline", regexp_get::<4>);
    getter(vm, rp, "sticky", regexp_get::<5>);
    getter(vm, rp, "unicode", regexp_get::<6>);
    getter(vm, rp, "dotAll", regexp_get::<7>);
    getter(vm, rp, "hasIndices", regexp_get::<8>);
}

/// `RegExp.prototype`'s getters: `source` (0), `flags` (1) and the flags
/// (2…); `undefined` (`source`: `(?:)`, `flags`: `""`) on the prototype
/// itself, a `TypeError` on anything but a `RegExp`.
fn regexp_get<const K: usize>(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    // (`flags`: generic, any object's.)
    if K == 1 {
        return regexp_flags_getter(vm, this);
    }
    if this.is_cell() && as_object(this) == vm.regexp_prototype {
        return Ok(match K {
            0 => str_value(vm, "(?:)"),
            1 => vm.new_string(&[]),
            _ => Value::UNDEFINED,
        });
    }
    let re = this_regexp(this)?;
    Ok(match K {
        0 => {
            let a = vm.a_source;
            vm.get(this, a)?
        }
        1 => str_value(vm, &regexp_flags(re)),
        2 => Value::from_bool(re.global),
        3 => Value::from_bool(re.ignore_case),
        4 => Value::from_bool(re.multiline),
        5 => Value::from_bool(re.sticky),
        6 => Value::from_bool(re.unicode),
        7 => Value::from_bool(re.dot_all),
        _ => Value::FALSE,
    })
}

/// A new `RegExp` of `pattern` and `flags` (compiled once per pair): its
/// own `lastIndex`, `source`, `global`, `ignoreCase`, `multiline`.
pub(crate) fn new_regexp(vm: &mut Vm, pattern: &str, flags: &str) -> Result<Value> {
    let key = (pattern.to_string(), flags.to_string());
    let re: *const crate::regexp::Regex = match vm.regexps.get(&key) {
        Some(r) => &**r,
        None => {
            let r = crate::regexp::Regex::new(pattern, flags).map_err(Error::new_syntax)?;
            let b = Box::new(r);
            let p: *const crate::regexp::Regex = &*b;
            vm.regexps.insert(key, b);
            p
        }
    };
    let source = if pattern.is_empty() { "(?:)".to_string() } else { escape_pattern(pattern) };
    let source = str_value(vm, &source);
    Ok(regexp_object(vm, re, source))
}

/// EscapeRegExpPattern: `pattern` as a literal's (`/…/`) would be: a `/`
/// outside a class escaped, line terminators as escapes.
fn escape_pattern(pattern: &str) -> String {
    if !pattern.contains(['/', '\n', '\r', '\u{2028}', '\u{2029}']) {
        return pattern.to_string();
    }
    let mut out = String::with_capacity(pattern.len() + 2);
    let (mut class, mut escaped) = (false, false);
    for c in pattern.chars() {
        let terminator = match c {
            '\n' => Some("\\n"),
            '\r' => Some("\\r"),
            '\u{2028}' => Some("\\u2028"),
            '\u{2029}' => Some("\\u2029"),
            _ => None,
        };
        if let Some(t) = terminator {
            // (An escaped one, `\` and the character: the escape alone.)
            if escaped {
                out.pop();
            }
            out.push_str(t);
            escaped = false;
            continue;
        }
        if c == '/' && !escaped && !class {
            out.push('\\');
        }
        out.push(c);
        if !escaped {
            match c {
                '[' => class = true,
                ']' => class = false,
                _ => {}
            }
        }
        escaped = !escaped && c == '\\';
    }
    out
}

/// A regular expression literal's object, its pattern and flags string
/// constants compiled once.
pub(crate) fn regexp_literal(vm: &mut Vm, pattern: Value, flags: Value) -> Result<Value> {
    let key = (pattern.to_bits(), flags.to_bits());
    let re = match vm.regexp_literals.get(&key) {
        Some(&re) => re,
        None => {
            let (p, f) = (String::from_utf16_lossy(&units(pattern)), String::from_utf16_lossy(&units(flags)));
            let made = new_regexp(vm, &p, &f)?;
            vm.regexp_literals.insert(key, regexp_of(made).unwrap());
            return Ok(made);
        }
    };
    Ok(regexp_object(vm, re, pattern))
}

/// A `RegExp` object for `re`, with `source`.
fn regexp_object(vm: &mut Vm, re: *const crate::regexp::Regex, source: Value) -> Value {
    let proto = object_value(vm.regexp_prototype);
    let o = vm.new_object_with(Class::RegExp, proto, 6);
    unsafe { (*o).code = re.cast() };
    let r = unsafe { &*re };
    let props = [
        (vm.a_last_index, Value::from_i32(0)),
        (vm.a_source, source),
        (vm.a_global, Value::from_bool(r.global)),
        (vm.a_ignore_case, Value::from_bool(r.ignore_case)),
        (vm.a_multiline, Value::from_bool(r.multiline)),
    ];
    // The shape with all five found once: then only their stores.
    let root = unsafe { (*o).shape };
    if vm.regexp_shapes.0 == root {
        unsafe { (*o).shape = vm.regexp_shapes.1 };
        for (slot, (_, v)) in props.into_iter().enumerate() {
            vm.store_field(o, slot as u32, v);
        }
        return object_value(o);
    }
    for (a, v) in props {
        let attrs = if a == vm.a_last_index { DONT_ENUM | DONT_DELETE } else { DONT_ENUM | READ_ONLY };
        vm.add_property_with(o, a, v, attrs);
    }
    if unsafe { !(*root).unique && !(*(*o).shape).unique } {
        vm.regexp_shapes = (root, unsafe { (*o).shape });
    }
    object_value(o)
}

/// `v`'s compiled regular expression, if it is a `RegExp`.
fn regexp_of<'a>(v: Value) -> Option<&'a crate::regexp::Regex> {
    is_class(v, Class::RegExp).then(|| unsafe { &*((*as_object(v)).code as *const crate::regexp::Regex) })
}

/// `RegExp.prototype.compile(pattern, flags)` (Annex B): `this` made
/// anew, of `pattern` (a string, or another `RegExp`'s source and flags).
fn regexp_compile(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !is_class(this, Class::RegExp) {
        return Err(Error::type_error("RegExp.prototype.compile called on an incompatible receiver"));
    }
    let (pattern, flags) = (arg(a, 0), arg(a, 1));
    let (p, f) = if let Some(re) = regexp_of(pattern) {
        if !flags.is_undefined() {
            return Err(Error::type_error("Cannot supply flags when constructing one RegExp from another"));
        }
        let a_source = vm.a_source;
        let src = vm.get(pattern, a_source)?;
        let src = String::from_utf16_lossy(string_units(src).unwrap_or(&[]));
        (if src == "(?:)" { String::new() } else { src }, regexp_flags(re))
    } else {
        let p = if pattern.is_undefined() {
            String::new()
        } else {
            String::from_utf16_lossy(string_units(crate::intrinsics::to_string(vm, pattern)?).unwrap_or(&[]))
        };
        let f = if flags.is_undefined() {
            String::new()
        } else {
            String::from_utf16_lossy(string_units(crate::intrinsics::to_string(vm, flags)?).unwrap_or(&[]))
        };
        (p, f)
    };
    let made = new_regexp(vm, &p, &f)?;
    let re = regexp_of(made).unwrap() as *const crate::regexp::Regex;
    let o = as_object(this);
    unsafe { (*o).code = re.cast() };
    let r = unsafe { &*re };
    let source = if p.is_empty() { "(?:)".to_string() } else { p };
    let source = str_value(vm, &source);
    for (k, v) in [
        (vm.a_source, source),
        (vm.a_global, Value::from_bool(r.global)),
        (vm.a_ignore_case, Value::from_bool(r.ignore_case)),
        (vm.a_multiline, Value::from_bool(r.multiline)),
    ] {
        let d = crate::props::Desc::data(v, false, false, true);
        let _ = vm.define_own_property(o, crate::vm::Key::Name(k), &d);
    }
    let a_last = vm.a_last_index;
    vm.put_key(this, crate::vm::Key::Name(a_last), Value::from_i32(0))?;
    Ok(this)
}

/// `RegExp(pattern, flags)`: from a string, or from another `RegExp`
/// (its source; its flags unless given).
fn regexp_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let p = arg(a, 0);
    // `RegExp(re)`, called: `re` itself if a regexp of `RegExp`'s.
    if !vm.is_construct() && arg(a, 1).is_undefined() && regexp_of(p).is_some() {
        let a_ctor = vm.a_constructor;
        let c = vm.get(p, a_ctor)?;
        let a_regexp = vm.intern("RegExp");
        if c == vm.get(object_value(vm.global), a_regexp)? {
            return Ok(p);
        }
    }
    let (mut pattern, mut flags) = (String::new(), String::new());
    if let Some(re) = regexp_of(p) {
        let a_source = vm.intern("source");
        pattern = String::from_utf16_lossy(&units(vm.get(p, a_source)?));
        flags = regexp_flags(re);
    } else if !p.is_undefined() {
        pattern = String::from_utf16_lossy(&units(p));
    }
    if pattern == "(?:)" {
        pattern.clear();
    }
    if !arg(a, 1).is_undefined() {
        flags = String::from_utf16_lossy(&units(arg(a, 1)));
    }
    new_regexp(vm, &pattern, &flags)
}

fn regexp_flags(re: &crate::regexp::Regex) -> String {
    let mut f = String::new();
    for (on, c) in [
        (re.global, 'g'),
        (re.ignore_case, 'i'),
        (re.multiline, 'm'),
        (re.dot_all, 's'),
        (re.unicode, 'u'),
        (re.sticky, 'y'),
    ] {
        if on {
            f.push(c);
        }
    }
    f
}

fn this_regexp<'a>(this: Value) -> Result<&'a crate::regexp::Regex> {
    regexp_of(this).ok_or_else(|| Error::type_error("RegExp method called on an incompatible receiver"))
}

/// The match of `re` (the object `this`) in `s` where `exec` looks: from
/// `lastIndex` if global (then set past the match, or to 0 on none).
fn regexp_match(vm: &mut Vm, this: Value, re: &crate::regexp::Regex, s: &[u16]) -> Result<Option<crate::regexp::Captures>> {
    let mut caps = Vec::new();
    Ok(regexp_match_into(vm, this, re, s, &mut caps)?.then_some(caps))
}

/// [`regexp_match`] into `caps`: whether it matched.
fn regexp_match_into(
    vm: &mut Vm,
    this: Value,
    re: &crate::regexp::Regex,
    s: &[u16],
    caps: &mut crate::regexp::Captures,
) -> Result<bool> {
    let a_last = vm.a_last_index;
    let plain = plain_last_index(vm, this);
    // (`lastIndex` read and converted even when not used.)
    let li = match plain {
        Some(o) => unsafe { *Object::slot(o, (*(*o).shape).location(0)) },
        None => vm.get(this, a_last)?,
    };
    let li = if li.is_int32() { li.as_i32_unchecked() as f64 } else { to_number(li) };
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let start = if re.global || re.sticky {
        if li.is_nan() || li < 0.0 { 0 } else { li as usize }
    } else {
        0
    };
    // (Stores that throw where `lastIndex` is read-only.)
    if start > s.len() {
        vm.put_key(this, crate::vm::Key::Name(a_last), Value::from_i32(0))?;
        return Ok(false);
    }
    let found = re.exec_into(s, start, caps);
    if re.global || re.sticky {
        let next = Value::from_i32(if found { caps[0].unwrap().1 } else { 0 } as i32);
        match plain {
            Some(o) => vm.store_field(o, 0, next),
            None => vm.put_key(this, crate::vm::Key::Name(a_last), next)?,
        }
    }
    Ok(found)
}

/// `this`, a `RegExp`, if its `lastIndex` is still the writable data
/// property in slot 0 it was made with: read and stored there directly.
fn plain_last_index(vm: &Vm, this: Value) -> Option<*mut Object> {
    let o = as_object(this);
    let sh = unsafe { &*(*o).shape };
    (sh.keys.first() == Some(&vm.a_last_index) && sh.attrs[0] == DONT_ENUM | DONT_DELETE).then_some(o)
}

/// `f` with the VM's captures buffer (see `Vm::caps_buf`).
fn with_caps<R>(vm: &mut Vm, f: impl FnOnce(&mut Vm, &mut crate::regexp::Captures) -> R) -> R {
    let mut caps = std::mem::take(&mut vm.caps_buf);
    let r = f(vm, &mut caps);
    vm.caps_buf = caps;
    r
}

/// The array `exec` returns for `caps` over `s`: the match and each group
/// (`undefined` if it didn't match), with `index` and `input`.
fn match_array(vm: &mut Vm, re: &crate::regexp::Regex, caps: &crate::regexp::Captures, s: &[u16], input: Value) -> Result<Value> {
    let arr = vm.new_array();
    let o = as_object(arr);
    vm.reserve_elems(o, caps.len());
    // The groups straight into its elements, the length counting each
    // (a collection while a string is made sees them), then the shape
    // for their type.
    let mut ty = crate::object::FieldType::None;
    for (i, c) in caps.iter().enumerate() {
        let v = match c {
            Some((a, b)) => string(vm, &s[*a..*b]),
            None => Value::UNDEFINED,
        };
        ty = ty.join(crate::object::FieldType::of(v));
        unsafe {
            *(*o).elems.add(i) = v;
            crate::heap::barrier((*o).elems.cast::<u8>().sub(8), (*o).elems.add(i), v);
            (*o).elems_len = i as u32 + 1;
        }
    }
    // Defined, as ECMAScript's CreateDataProperty does (no setters), along
    // transitions found once per type of elements.
    let (elems, with_index, with_input, with_groups) = match vm.match_shapes.iter().find(|m| m.0 == ty) {
        Some(&(_, e, a, b, c)) => (e, a, b, c),
        None => {
            let e = vm.elements_transition(vm.array_shape, ty);
            let a = vm.transition_with(e, vm.a_index, 0);
            let b = vm.transition_with(a, vm.a_input, 0);
            let a_groups = vm.intern("groups");
            let c = vm.transition_with(b, a_groups, 0);
            if vm.match_shapes.len() == 4 {
                vm.match_shapes.remove(0);
            }
            vm.match_shapes.push((ty, e, a, b, c));
            (e, a, b, c)
        }
    };
    unsafe { (*o).shape = elems };
    vm.add_property_shaped(o, with_index, Value::from_i32(caps[0].unwrap().0 as i32));
    vm.add_property_shaped(o, with_input, input);
    // `groups`: with named groups, each one's match.
    let groups = if re.names.is_empty() { Value::UNDEFINED } else { named_groups(vm, re, caps, s)? };
    vm.add_property_shaped(o, with_groups, groups);
    Ok(arr)
}

/// A match's named groups: a null-prototype object of their matches
/// (`undefined` where a group didn't match).
fn named_groups(vm: &mut Vm, re: &crate::regexp::Regex, caps: &[Option<(usize, usize)>], s: &[u16]) -> Result<Value> {
    let groups = object_value(vm.new_object(Value::NULL));
    for (name, g) in &re.names {
        let v = match caps.get(*g).copied().flatten() {
            Some((a, b)) => string(vm, &s[a..b]),
            None => Value::UNDEFINED,
        };
        let key = vm.intern(name);
        vm.create_data_property(groups, crate::vm::Key::Name(key), v)?;
    }
    Ok(groups)
}

fn regexp_exec(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let re = this_regexp(this)?;
    // (No argument: "undefined".)
    let input = string_fn(vm, Value::UNDEFINED, &[arg(a, 0)])?;
    // Borrowed: strings don't change, and the stack scan keeps `input`.
    let s = units(input);
    with_caps(vm, |vm, caps| {
        if regexp_match_into(vm, this, re, &s, caps)? {
            match_array(vm, re, caps, &s, input)
        } else {
            Ok(Value::NULL)
        }
    })
}

fn regexp_test(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    // (Through a user's `exec`, as the spec has it.)
    if crate::props::is_object(this) && !pristine(vm, this)? {
        let s = string_fn(vm, Value::UNDEFINED, &[arg(a, 0)])?;
        return Ok(Value::from_bool(!rx_exec(vm, this, s)?.is_null()));
    }
    let re = this_regexp(this)?;
    let s = units(arg(a, 0));
    with_caps(vm, |vm, caps| Ok(Value::from_bool(regexp_match_into(vm, this, re, &s, caps)?)))
}

fn regexp_to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let re = this_regexp(this)?;
    let a_source = vm.intern("source");
    let src = String::from_utf16_lossy(&units(vm.get(this, a_source)?));
    Ok(str_value(vm, &format!("/{src}/{}", regexp_flags(re))))
}

/// The `RegExp` a String method's argument stands for (a string is a
/// pattern).
fn as_regexp(vm: &mut Vm, v: Value) -> Result<Value> {
    if regexp_of(v).is_some() {
        return Ok(v);
    }
    let p = if v.is_undefined() { String::new() } else { String::from_utf16_lossy(&units(v)) };
    new_regexp(vm, &p, "")
}

/// `s.matchAll(re)`: an iterator over `exec`'s arrays for each match (a
/// `RegExp` must be global; anything else is a pattern made one). The
/// matches are found at once, from the `RegExp`'s `lastIndex`.
/// `RegExp.prototype[Symbol.match]` (`K` 0), `[Symbol.matchAll]` (1),
/// `[Symbol.replace]` (2), `[Symbol.search]` (3), `[Symbol.split]` (4):
/// the String method, of the string argument, with this RegExp.
#[cold]
#[inline(never)]
fn regexp_symbol<const K: usize>(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::props::is_object(this) {
        return Err(Error::type_error("RegExp method called on an incompatible receiver"));
    }
    let s = string_fn(vm, Value::UNDEFINED, &[arg(a, 0)])?;
    // (One whose `exec` isn't the builtin one, or no RegExp: as the spec
    // has it, through `exec`.)
    if !pristine(vm, this)? {
        return match K {
            0 => rx_match(vm, this, s),
            1 => rx_match_all(vm, this, s),
            2 => rx_replace(vm, this, s, arg(a, 1)),
            3 => rx_search(vm, this, s),
            _ => rx_split(vm, this, s, arg(a, 1)),
        };
    }
    let mut args = vec![this];
    args.extend_from_slice(a.get(1..).unwrap_or(&[]));
    match K {
        0 => string_match(vm, s, &args),
        1 => string_match_all(vm, s, &args),
        2 => string_replace(vm, s, &args),
        3 => string_search(vm, s, &args),
        _ => string_split(vm, s, &args),
    }
}

/// A String method's pattern `a[0]`, if an object (not a RegExp) with a
/// `Symbol.<name>` method: that method's result, of `this` and the rest.
fn delegate(vm: &mut Vm, name: &str, this: Value, a: &[Value]) -> Result<Option<Value>> {
    let k = ["match", "matchAll", "replace", "search", "split"].iter().position(|&n| n == name).unwrap_or(0);
    let pattern = arg(a, 0);
    if !crate::props::is_object(pattern) {
        return Ok(None);
    }
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error("String.prototype method called on null or undefined"));
    }
    // (A RegExp's builtin method with the builtin `exec`: the fast path.)
    if pristine_for(vm, pattern, Some(k))? {
        return Ok(None);
    }
    let atom = vm.regexp_atoms[1 + k];
    let m = vm.get(pattern, atom)?;
    if m.is_undefined() || m.is_null() {
        return Ok(None);
    }
    let mut args = vec![this];
    args.extend_from_slice(a.get(1..).unwrap_or(&[]));
    crate::interp::call_function(vm, m, pattern, &args).map(Some)
}

fn string_match_all(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if let Some(r) = delegate(vm, "matchAll", this, a)? {
        return Ok(r);
    }
    let s = this_units(this)?;
    let pattern = arg(a, 0);
    let rv = match regexp_of(pattern) {
        Some(re) if !re.global => {
            return Err(Error::type_error("String.prototype.matchAll called with a non-global RegExp argument"));
        }
        Some(_) => pattern,
        None => {
            let p = if pattern.is_undefined() { String::new() } else { String::from_utf16_lossy(&units(pattern)) };
            if vm.conv_failed() {
                return Err(vm.take_conv_error());
            }
            new_regexp(vm, &p, "g")?
        }
    };
    let re = regexp_of(rv).unwrap();
    let a_last = vm.a_last_index;
    let li = vm.get(rv, a_last)?;
    let li = to_number(li);
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let input = this_string(vm, this, &s);
    let out = vm.new_array();
    let mut at = if li.is_nan() || li < 0.0 { 0 } else { li.min(s.len() as f64 + 1.0) as usize };
    let mut caps = Vec::new();
    while at <= s.len() && re.exec_into(&s, at, &mut caps) {
        let (x, y) = caps[0].unwrap();
        at = if y == x { y + 1 } else { y };
        let m = match_array(vm, re, &caps, &s, input)?;
        crate::vm::array_push(vm, out, &[m])?;
    }
    crate::symbol::array_values(vm, out, &[])
}

/// `s.match(re)`: `exec`'s array, or with `g` every match (`null` if
/// none).
fn string_match(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if let Some(r) = delegate(vm, "match", this, a)? {
        return Ok(r);
    }
    let s = this_units(this)?;
    let rv = as_regexp(vm, arg(a, 0))?;
    let re = regexp_of(rv).unwrap();
    let input = this_string(vm, this, &s);
    if !re.global {
        return with_caps(vm, |vm, caps| {
            if regexp_match_into(vm, rv, re, &s, caps)? {
                match_array(vm, re, caps, &s, input)
            } else {
                Ok(Value::NULL)
            }
        });
    }
    let mut found = Vec::new();
    let mut at = 0;
    while let Some(caps) = re.exec(&s, at) {
        let (x, y) = caps[0].unwrap();
        found.push(string(vm, &s[x..y]));
        at = if y == x { y + 1 } else { y };
        if at > s.len() {
            break;
        }
    }
    let a_last = vm.intern("lastIndex");
    vm.set(rv, a_last, Value::from_i32(0))?;
    if found.is_empty() {
        return Ok(Value::NULL);
    }
    let arr = vm.new_array();
    crate::vm::array_push(vm, arr, &found)?;
    Ok(arr)
}

fn string_search(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if let Some(r) = delegate(vm, "search", this, a)? {
        return Ok(r);
    }
    let s = this_units(this)?;
    let rv = as_regexp(vm, arg(a, 0))?;
    let re = regexp_of(rv).unwrap();
    Ok(Value::from_i32(re.exec(&s, 0).map_or(-1, |c| c[0].unwrap().0 as i32)))
}

/// `s.replace(pattern, replacement)`: the first match of a string
/// pattern, or of a regular expression (every one with `g`), replaced by
/// a string (with `$&`, `$1`…, `$$`, `` $` ``, `$'`) or a function's
/// result (called with the match, the groups, the index and `s`).
fn string_replace(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if let Some(r) = delegate(vm, "replace", this, a)? {
        return Ok(r);
    }
    replace(vm, this, a, false)
}

/// `s.replaceAll(pattern, replacement)`: every match replaced (a
/// `RegExp` pattern must be global).
fn string_replace_all(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if let Some(r) = delegate(vm, "replace", this, a)? {
        return Ok(r);
    }
    if regexp_of(arg(a, 0)).is_some_and(|re| !re.global) {
        return Err(Error::type_error("replaceAll must be called with a global RegExp"));
    }
    replace(vm, this, a, true)
}

/// `replace` (`all`: `replaceAll`).
fn replace(vm: &mut Vm, this: Value, a: &[Value], all: bool) -> Result<Value> {
    let s = this_units(this)?;
    let pattern = arg(a, 0);
    let replacement = arg(a, 1);
    // (The pattern's string before the replacement's.)
    let pattern_units = if regexp_of(pattern).is_none() { Some(units(pattern).into_owned()) } else { None };
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let func = replacement.is_cell()
        && matches!(unsafe { (*as_object(replacement)).class }, Class::Function | Class::Native);
    let template = if func { std::borrow::Cow::Borrowed(&[][..]) } else { units(replacement) };
    // The matches: each one's groups.
    let mut flat: crate::regexp::Captures = Vec::new();
    let mut stride = 1;
    let mut caps = std::mem::take(&mut vm.caps_buf);
    match regexp_of(pattern) {
        Some(re) => {
            if re.global {
                let mut at = 0;
                stride = re.groups;
                while at <= s.len() && re.exec_into(&s, at, &mut caps) {
                    let (x, y) = caps[0].unwrap();
                    at = if y == x { y + 1 } else { y };
                    flat.extend_from_slice(&caps);
                }
                let a_last = vm.intern("lastIndex");
                vm.set(pattern, a_last, Value::from_i32(0))?;
            } else if let Some(c) = regexp_match(vm, pattern, re, &s)? {
                stride = c.len();
                flat.extend_from_slice(&c);
            }
        }
        None => {
            let p = pattern_units.unwrap_or_default();
            let mut at = 0;
            while let Some(i) = find(&s, &p, at) {
                flat.push(Some((i, i + p.len())));
                // (An empty pattern matches between each unit.)
                at = i + p.len().max(1);
                if !all || at > s.len() {
                    break;
                }
            }
        }
    }
    vm.caps_buf = caps;
    if flat.is_empty() {
        return Ok(this_string(vm, this, &s));
    }
    let input = this_string(vm, this, &s);
    let mut out = Vec::with_capacity(s.len());
    let mut last = 0;
    for caps in flat.chunks(stride) {
        let (x, y) = caps[0].unwrap();
        out.extend_from_slice(&s[last..x]);
        if func {
            let mut args: Vec<Value> = caps
                .iter()
                .map(|c| match c {
                    Some((p, q)) => string(vm, &s[*p..*q]),
                    None => Value::UNDEFINED,
                })
                .collect();
            args.push(Value::from_i32(x as i32));
            args.push(input);
            // (With named groups, their object last.)
            if let Some(re) = regexp_of(pattern).filter(|re| !re.names.is_empty()) {
                let g = named_groups(vm, re, caps, &s)?;
                args.push(g);
            }
            // (The captures are only in `args`: roots for the call.)
            let r = vm.handle_scope(|vm| {
                vm.handles.extend_from_slice(&args);
                crate::interp::call_function(vm, replacement, Value::UNDEFINED, &args)
            })?;
            out.extend_from_slice(&units(r));
        } else {
            let names = regexp_of(pattern).map_or(&[][..], |re| &re.names[..]);
            expand(&template, &s, caps, names, &mut out);
        }
        last = y;
    }
    out.extend_from_slice(&s[last..]);
    Ok(vm.new_string(&out))
}

/// A replacement template's `$` patterns expanded for one match.
fn expand(t: &[u16], s: &[u16], caps: &[Option<(usize, usize)>], names: &[(String, usize)], out: &mut Vec<u16>) {
    let (x, y) = caps[0].unwrap();
    let mut i = 0;
    while i < t.len() {
        let c = t[i];
        if c != b'$' as u16 || i + 1 == t.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = t[i + 1];
        match n as u8 {
            b'$' => {
                out.push(b'$' as u16);
                i += 2;
            }
            b'&' => {
                out.extend_from_slice(&s[x..y]);
                i += 2;
            }
            b'`' => {
                out.extend_from_slice(&s[..x]);
                i += 2;
            }
            b'\'' => {
                out.extend_from_slice(&s[y..]);
                i += 2;
            }
            // `$<name>`, with named groups: that group's match.
            b'<' if !names.is_empty() => match t[i + 2..].iter().position(|&c| c == b'>' as u16) {
                Some(k) => {
                    let name = String::from_utf16_lossy(&t[i + 2..i + 2 + k]);
                    if let Some(&(_, g)) = names.iter().find(|(n, _)| *n == name)
                        && let Some((p, q)) = caps[g]
                    {
                        out.extend_from_slice(&s[p..q]);
                    }
                    i += k + 3;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            b'0'..=b'9' if n < 128 => {
                // `$nn` if that group exists, else `$n`.
                let d1 = (n - b'0' as u16) as usize;
                let two = t.get(i + 2).filter(|&&c| (b'0' as u16..=b'9' as u16).contains(&c));
                let (g, len) = match two {
                    Some(&d2) if d1 * 10 + ((d2 - b'0' as u16) as usize) < caps.len() && d1 * 10 + ((d2 - b'0' as u16) as usize) > 0 => {
                        (d1 * 10 + (d2 - b'0' as u16) as usize, 3)
                    }
                    _ => (d1, 2),
                };
                if g == 0 || g >= caps.len() {
                    out.push(c);
                    i += 1;
                    continue;
                }
                if let Some((p, q)) = caps[g] {
                    out.extend_from_slice(&s[p..q]);
                }
                i += len;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
}

/// `split` by a regular expression: the pieces between matches (not
/// empty ones at the very start or end), each match's groups after its
/// piece.
fn regexp_split(vm: &mut Vm, re: &crate::regexp::Regex, s: &[u16], limit: Option<Value>) -> Result<Value> {
    let limit = match limit {
        Some(v) if !v.is_undefined() => crate::interp::to_int32(to_number(v)) as u32 as usize,
        _ => usize::MAX,
    };
    let mut parts: Vec<Value> = Vec::new();
    if limit == 0 {
        let arr = vm.new_array();
        return Ok(arr);
    }
    if s.is_empty() {
        if re.exec(s, 0).is_none() {
            parts.push(vm.new_string(&[]));
        }
    } else {
        let (mut p, mut q) = (0, 0);
        while q < s.len() {
            match re.exec(s, q) {
                None => break,
                Some(caps) => {
                    let (x, e) = caps[0].unwrap();
                    if x >= s.len() {
                        break;
                    }
                    if e == p || (e == x && x == q && e == p) {
                        q = x + 1;
                        continue;
                    }
                    if e == x && x == p {
                        q = x + 1;
                        continue;
                    }
                    parts.push(string(vm, &s[p..x]));
                    if parts.len() == limit {
                        break;
                    }
                    for c in &caps[1..] {
                        parts.push(match c {
                            Some((a, b)) => string(vm, &s[*a..*b]),
                            None => Value::UNDEFINED,
                        });
                        if parts.len() == limit {
                            break;
                        }
                    }
                    p = e;
                    q = if e == x { e + 1 } else { e };
                }
            }
        }
        if parts.len() < limit {
            parts.push(string(vm, &s[p..]));
        }
    }
    let arr = vm.new_array();
    crate::vm::array_push(vm, arr, &parts)?;
    Ok(arr)
}

// ---- Errors ----

/// `new Error(message)` (or a call) for kind `K` of [`ERROR_KINDS`].
fn error_ctor<const K: usize>(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let message = match a.first() {
        Some(v) if !v.is_undefined() => Some(units(*v).into_owned()),
        _ => None,
    };
    let e = make_error(vm, K, message.as_deref());
    // `{ cause }` in the options: its own `cause`.
    let options = arg(a, 1);
    if crate::props::is_object(options) {
        let a_cause = vm.intern("cause");
        if vm.has_key(options, crate::vm::Key::Name(a_cause))? {
            let cause = vm.get(options, a_cause)?;
            vm.add_property_with(as_object(e), a_cause, cause, DONT_ENUM);
        }
    }
    Ok(e)
}

/// A new error object of kind `k`, with its own `message` if given.
pub(crate) fn make_error(vm: &mut Vm, k: usize, message: Option<&[u16]>) -> Value {
    let proto = vm.error_prototypes[k];
    let o = vm.new_object(object_value(proto));
    unsafe { (*o).flags |= crate::object::ERROR };
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
    if !crate::props::is_object(this) {
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

// ---- RegExp's generic algorithms (through `exec`) ----

/// Whether `rx` is a RegExp whose `exec` is the builtin one: its methods'
/// fast paths then do what the generic algorithms would.
pub(crate) fn pristine(vm: &mut Vm, rx: Value) -> Result<bool> {
    pristine_for(vm, rx, None)
}

/// [`pristine`], and its `Symbol.<k>` method (`regexp_fns[1 + k]`) the
/// builtin one too. Quick for a RegExp of the usual shape while
/// `RegExp.prototype` keeps the shape it had when its methods were last
/// found the builtin ones (the slots then read where they were).
#[inline(always)]
fn pristine_for(vm: &mut Vm, rx: Value, k: Option<usize>) -> Result<bool> {
    if !is_class(rx, Class::RegExp) {
        return Ok(false);
    }
    unsafe {
        let rp = vm.regexp_prototype;
        if (*as_object(rx)).shape == vm.regexp_shapes.1 && (*rp).shape == vm.regexp_proto_shape {
            let at = |i: usize| *Object::slot(rp, vm.regexp_slots[i]);
            let ok = |i: usize| at(i) == object_value(vm.regexp_fns[i]);
            return Ok(ok(0) && k.is_none_or(|k| ok(1 + k)));
        }
    }
    pristine_slow(vm, rx, k)
}

/// [`pristine_for`] by lookups (the prototype's slots noted, for its
/// quick check).
#[cold]
#[inline(never)]
fn pristine_slow(vm: &mut Vm, rx: Value, k: Option<usize>) -> Result<bool> {
    let a = vm.regexp_atoms[0];
    let e = vm.get(rx, a)?;
    if !(e.is_cell() && as_object(e) == vm.regexp_fns[0]) {
        return Ok(false);
    }
    if let Some(k) = k {
        let m = vm.get(rx, vm.regexp_atoms[1 + k])?;
        if !(m.is_cell() && as_object(m) == vm.regexp_fns[1 + k]) {
            return Ok(false);
        }
    }
    // (Where the prototype's methods are, for the next time.)
    unsafe {
        let rp = vm.regexp_prototype;
        let s = &*(*rp).shape;
        let mut slots = [0; 6];
        for (i, slot) in slots.iter_mut().enumerate() {
            match s.lookup(vm.regexp_atoms[i]) {
                Some(j) if *Object::slot(rp, s.location(j)) == object_value(vm.regexp_fns[i]) => *slot = s.location(j),
                _ => return Ok(true),
            }
        }
        vm.regexp_slots = slots;
        vm.regexp_proto_shape = (*rp).shape;
    }
    Ok(true)
}

/// RegExpExec(r, s): `r.exec(s)` (an object or null), or the builtin's.
#[cold]
#[inline(never)]
fn rx_exec(vm: &mut Vm, r: Value, s: Value) -> Result<Value> {
    let a = vm.intern("exec");
    let exec = vm.get(r, a)?;
    if crate::props::is_callable(exec) {
        let res = crate::interp::call_function(vm, exec, r, &[s])?;
        if !crate::props::is_object(res) && !res.is_null() {
            return Err(Error::type_error("exec result must be an object or null"));
        }
        return Ok(res);
    }
    if regexp_of(r).is_none() {
        return Err(Error::type_error("RegExp method called on an incompatible receiver"));
    }
    regexp_exec(vm, r, &[s])
}

/// ToString(Get(o, name)), as units.
#[cold]
#[inline(never)]
fn get_units(vm: &mut Vm, o: Value, name: &str) -> Result<Vec<u16>> {
    let v = get_named(vm, o, name)?;
    let s = crate::intrinsics::to_string(vm, v)?;
    Ok(string_units(s).unwrap_or(&[]).to_vec())
}

/// Get(o, name), `name` an index (`"0"`) or a name.
#[cold]
#[inline(never)]
fn get_named(vm: &mut Vm, o: Value, name: &str) -> Result<Value> {
    match name.parse::<i32>() {
        Ok(i) => vm.get_keyed(o, Value::from_i32(i)),
        Err(_) => {
            let a = vm.intern(name);
            vm.get(o, a)
        }
    }
}

/// ToLength(Get(o, name)).
#[cold]
#[inline(never)]
fn get_length(vm: &mut Vm, o: Value, name: &str) -> Result<f64> {
    let v = get_named(vm, o, name)?;
    let d = to_number(v);
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    Ok(if d.is_nan() || d <= 0.0 { 0.0 } else { d.trunc().min(9007199254740991.0) })
}

#[cold]
#[inline(never)]
fn set_last_index(vm: &mut Vm, r: Value, v: f64) -> Result<()> {
    let a = vm.a_last_index;
    vm.put_key(r, crate::vm::Key::Name(a), Value::number(v))
}

/// AdvanceStringIndex(s, i, unicode).
#[cold]
#[inline(never)]
fn advance(s: &[u16], i: f64, unicode: bool) -> f64 {
    let k = i as usize;
    if unicode && k + 1 < s.len() && (0xD800..0xDC00).contains(&s[k]) && (0xDC00..0xE000).contains(&s[k + 1]) {
        i + 2.0
    } else {
        i + 1.0
    }
}

/// Whether a `flags` string has `c` (and `u` or `v`, unicode).
#[cold]
#[inline(never)]
fn flags_have(f: &[u16], c: u8) -> bool {
    // (Not `contains`: a second caller of `[u16]`'s took it out of line,
    // and the regex search's with it: RegExp 8% slower.)
    f.iter().any(|&x| x == c as u16)
}

/// `RegExp.prototype[Symbol.match]` generically.
#[cold]
#[inline(never)]
fn rx_match(vm: &mut Vm, r: Value, s: Value) -> Result<Value> {
    let flags = get_units(vm, r, "flags")?;
    if !flags_have(&flags, b'g') {
        return rx_exec(vm, r, s);
    }
    let unicode = flags_have(&flags, b'u') || flags_have(&flags, b'v');
    set_last_index(vm, r, 0.0)?;
    let su = string_units(s).unwrap_or(&[]).to_vec();
    let out = vm.new_array();
    let mut n = 0;
    loop {
        let res = rx_exec(vm, r, s)?;
        if res.is_null() {
            return Ok(if n == 0 { Value::NULL } else { out });
        }
        let m = get_units(vm, res, "0")?;
        let mv = vm.new_string(&m);
        crate::vm::array_push(vm, out, &[mv])?;
        n += 1;
        if m.is_empty() {
            let i = get_length(vm, r, "lastIndex")?;
            set_last_index(vm, r, advance(&su, i, unicode))?;
        }
    }
}

/// `RegExp.prototype[Symbol.search]` generically.
#[cold]
#[inline(never)]
fn rx_search(vm: &mut Vm, r: Value, s: Value) -> Result<Value> {
    let a = vm.a_last_index;
    let prev = vm.get(r, a)?;
    // (SameValue by bits: a number that is 0 is the int32 0 or ±0.0;
    // keeping `same_value` to its callers keeps it inlined there.)
    let same = |x: Value, y: Value| x.to_bits() == y.to_bits() || x.is_number() && y.is_number() && x.to_number() == y.to_number() && x.to_number().is_sign_negative() == y.to_number().is_sign_negative();
    if !same(prev, Value::from_i32(0)) {
        set_last_index(vm, r, 0.0)?;
    }
    let res = rx_exec(vm, r, s)?;
    let cur = vm.get(r, a)?;
    if !same(cur, prev) {
        vm.put_key(r, crate::vm::Key::Name(a), prev)?;
    }
    if res.is_null() {
        return Ok(Value::from_i32(-1));
    }
    let ai = vm.intern("index");
    vm.get(res, ai)
}

/// GetSubstitution of `template` for match `matched` at `position` in
/// `s`, its captures `caps` (`None`: undefined) and named groups `named`
/// (an object, or undefined).
#[cold]
#[inline(never)]
fn substitution(
    vm: &mut Vm,
    matched: &[u16],
    s: &[u16],
    position: usize,
    caps: &[Option<Vec<u16>>],
    named: Value,
    t: &[u16],
) -> Result<Vec<u16>> {
    let mut out = Vec::new();
    let mut i = 0;
    let tail = (position + matched.len()).min(s.len());
    while i < t.len() {
        let c = t[i];
        if c != b'$' as u16 || i + 1 == t.len() {
            out.push(c);
            i += 1;
            continue;
        }
        let n = t[i + 1];
        match n {
            0x24 => {
                out.push(0x24);
                i += 2;
            }
            0x26 => {
                out.extend_from_slice(matched);
                i += 2;
            }
            0x60 => {
                out.extend_from_slice(&s[..position.min(s.len())]);
                i += 2;
            }
            0x27 => {
                out.extend_from_slice(&s[tail..]);
                i += 2;
            }
            0x30..=0x39 => {
                let d1 = (n - 0x30) as usize;
                let two = t.get(i + 2).filter(|&&d| (0x30..=0x39).contains(&d)).map(|&d| d1 * 10 + (d - 0x30) as usize);
                let (g, len) = match two {
                    Some(g) if g >= 1 && g <= caps.len() => (g, 3),
                    _ => (d1, 2),
                };
                if g == 0 || g > caps.len() {
                    out.push(c);
                    i += 1;
                    continue;
                }
                if let Some(cap) = &caps[g - 1] {
                    out.extend_from_slice(cap);
                }
                i += len;
            }
            0x3C if !named.is_undefined() => match t[i + 2..].iter().position(|&c| c == b'>' as u16) {
                Some(k) => {
                    let name = vm.new_string(&t[i + 2..i + 2 + k]);
                    let key = crate::intrinsics::to_property_key(vm, name)?;
                    let v = vm.get_key(named, key)?;
                    if !v.is_undefined() {
                        let sv = crate::intrinsics::to_string(vm, v)?;
                        out.extend_from_slice(string_units(sv).unwrap_or(&[]));
                    }
                    i += k + 3;
                }
                None => {
                    out.push(c);
                    i += 1;
                }
            },
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// `RegExp.prototype[Symbol.replace]` generically.
#[cold]
#[inline(never)]
fn rx_replace(vm: &mut Vm, r: Value, s: Value, replace: Value) -> Result<Value> {
    let su = string_units(s).unwrap_or(&[]).to_vec();
    let functional = crate::props::is_callable(replace);
    let template = if functional {
        Vec::new()
    } else {
        let t = crate::intrinsics::to_string(vm, replace)?;
        string_units(t).unwrap_or(&[]).to_vec()
    };
    let flags = get_units(vm, r, "flags")?;
    let global = flags_have(&flags, b'g');
    let unicode = flags_have(&flags, b'u') || flags_have(&flags, b'v');
    if global {
        set_last_index(vm, r, 0.0)?;
    }
    let results = vm.handle_scope(|vm| -> Result<Vec<Value>> {
        let mut results = Vec::new();
        loop {
            let res = rx_exec(vm, r, s)?;
            if res.is_null() {
                break;
            }
            vm.handles.push(res);
            results.push(res);
            if !global {
                break;
            }
            let m = get_units(vm, res, "0")?;
            if m.is_empty() {
                let i = get_length(vm, r, "lastIndex")?;
                set_last_index(vm, r, advance(&su, i, unicode))?;
            }
        }
        Ok(results)
    })?;
    let mut out = Vec::new();
    let mut next = 0;
    for res in results {
        let n = get_length(vm, res, "length")?;
        let ncaps = (n - 1.0).max(0.0) as usize;
        let matched = get_units(vm, res, "0")?;
        let ai = vm.intern("index");
        let pv = vm.get(res, ai)?;
        let p = to_number(pv);
        let position = if p.is_nan() { 0 } else { p.max(0.0).min(su.len() as f64) as usize };
        let mut caps = Vec::with_capacity(ncaps);
        for k in 1..=ncaps {
            let v = vm.get_keyed(res, Value::from_i32(k as i32))?;
            caps.push(if v.is_undefined() {
                None
            } else {
                let sv = crate::intrinsics::to_string(vm, v)?;
                Some(string_units(sv).unwrap_or(&[]).to_vec())
            });
        }
        let ag = vm.intern("groups");
        let named = vm.get(res, ag)?;
        let replacement = if functional {
            let mut args = vec![vm.new_string(&matched)];
            for c in &caps {
                args.push(match c {
                    Some(u) => vm.new_string(u),
                    None => Value::UNDEFINED,
                });
            }
            args.push(Value::from_i32(position as i32));
            args.push(s);
            if !named.is_undefined() {
                args.push(named);
            }
            let v = vm.handle_scope(|vm| {
                vm.handles.extend_from_slice(&args);
                crate::interp::call_function(vm, replace, Value::UNDEFINED, &args)
            })?;
            let sv = crate::intrinsics::to_string(vm, v)?;
            string_units(sv).unwrap_or(&[]).to_vec()
        } else {
            let named = if named.is_undefined() { named } else { object_value(vm.to_object(named)?) };
            substitution(vm, &matched, &su, position, &caps, named, &template)?
        };
        if position >= next {
            out.extend_from_slice(&su[next..position]);
            out.extend_from_slice(&replacement);
            next = (position + matched.len()).min(su.len());
        }
    }
    out.extend_from_slice(&su[next.min(su.len())..]);
    Ok(vm.new_string(&out))
}

/// SpeciesConstructor(r, %RegExp%).
#[cold]
#[inline(never)]
fn rx_species(vm: &mut Vm, r: Value) -> Result<Value> {
    let a_c = vm.a_constructor;
    let regexp = vm.get(object_value(vm.regexp_prototype), a_c)?;
    let c = vm.get(r, a_c)?;
    if c.is_undefined() {
        return Ok(regexp);
    }
    if !crate::props::is_object(c) {
        return Err(Error::type_error("object.constructor is not an object"));
    }
    let a_s = vm.intern(&format!("{}Symbol.species", crate::symbol::SYMBOL_MARK));
    let sp = vm.get(c, a_s)?;
    if sp.is_undefined() || sp.is_null() {
        return Ok(regexp);
    }
    // (A constructor's check through `construct`, which the caller does:
    // its own callers keep `is_constructor` inlined.)
    if !crate::props::is_callable(sp) {
        return Err(Error::type_error("object.constructor[Symbol.species] is not a constructor"));
    }
    Ok(sp)
}

/// `RegExp.prototype[Symbol.split]` generically: a sticky copy (of the
/// species constructor) tried at each position.
#[cold]
#[inline(never)]
fn rx_split(vm: &mut Vm, r: Value, s: Value, limit: Value) -> Result<Value> {
    let c = rx_species(vm, r)?;
    let mut flags = get_units(vm, r, "flags")?;
    let unicode = flags_have(&flags, b'u') || flags_have(&flags, b'v');
    if !flags_have(&flags, b'y') {
        flags.push(b'y' as u16);
    }
    let fv = vm.new_string(&flags);
    let splitter = crate::intrinsics::construct(vm, c, &[r, fv])?;
    let out = vm.new_array();
    let lim = if limit.is_undefined() {
        u32::MAX as f64
    } else {
        let d = to_number(limit);
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
        crate::interp::to_int32(d) as u32 as f64
    };
    if lim == 0.0 {
        return Ok(out);
    }
    let su = string_units(s).unwrap_or(&[]).to_vec();
    let size = su.len();
    let mut n = 0.0;
    if size == 0 {
        if rx_exec(vm, splitter, s)?.is_null() {
            crate::vm::array_push(vm, out, &[s])?;
        }
        return Ok(out);
    }
    let (mut p, mut q) = (0usize, 0usize);
    while q < size {
        set_last_index(vm, splitter, q as f64)?;
        let z = rx_exec(vm, splitter, s)?;
        if z.is_null() {
            q = advance(&su, q as f64, unicode) as usize;
            continue;
        }
        let e = (get_length(vm, splitter, "lastIndex")? as usize).min(size);
        if e == p {
            q = advance(&su, q as f64, unicode) as usize;
            continue;
        }
        let part = vm.new_string(&su[p..q]);
        crate::vm::array_push(vm, out, &[part])?;
        n += 1.0;
        if n == lim {
            return Ok(out);
        }
        p = e;
        let ncaps = (get_length(vm, z, "length")? - 1.0).max(0.0) as usize;
        for k in 1..=ncaps {
            let v = vm.get_keyed(z, Value::from_i32(k as i32))?;
            crate::vm::array_push(vm, out, &[v])?;
            n += 1.0;
            if n == lim {
                return Ok(out);
            }
        }
        q = p;
    }
    let part = vm.new_string(&su[p.min(size)..]);
    crate::vm::array_push(vm, out, &[part])?;
    Ok(out)
}

/// `RegExp.prototype[Symbol.matchAll]` generically: a copy (of the
/// species constructor) from `lastIndex`, its matches found at once.
#[cold]
#[inline(never)]
fn rx_match_all(vm: &mut Vm, r: Value, s: Value) -> Result<Value> {
    let c = rx_species(vm, r)?;
    let flags = get_units(vm, r, "flags")?;
    let fv = vm.new_string(&flags);
    let matcher = crate::intrinsics::construct(vm, c, &[r, fv])?;
    let li = get_length(vm, r, "lastIndex")?;
    set_last_index(vm, matcher, li)?;
    let global = flags_have(&flags, b'g');
    let unicode = flags_have(&flags, b'u') || flags_have(&flags, b'v');
    let su = string_units(s).unwrap_or(&[]).to_vec();
    let out = vm.new_array();
    loop {
        let res = rx_exec(vm, matcher, s)?;
        if res.is_null() {
            break;
        }
        crate::vm::array_push(vm, out, &[res])?;
        if !global {
            break;
        }
        let m = get_units(vm, res, "0")?;
        if m.is_empty() {
            let i = get_length(vm, matcher, "lastIndex")?;
            set_last_index(vm, matcher, advance(&su, i, unicode))?;
        }
    }
    crate::symbol::array_values(vm, out, &[])
}

/// `get RegExp.prototype.flags`: each flag's getter asked.
#[cold]
#[inline(never)]
fn regexp_flags_getter(vm: &mut Vm, this: Value) -> Result<Value> {
    if !crate::props::is_object(this) {
        return Err(Error::type_error("RegExp.prototype.flags getter called on non-object"));
    }
    let mut out = String::new();
    for (name, c) in [
        ("hasIndices", 'd'),
        ("global", 'g'),
        ("ignoreCase", 'i'),
        ("multiline", 'm'),
        ("dotAll", 's'),
        ("unicode", 'u'),
        ("unicodeSets", 'v'),
        ("sticky", 'y'),
    ] {
        let a = vm.intern(name);
        if crate::interp::to_boolean(vm.get(this, a)?) {
            out.push(c);
        }
    }
    Ok(str_value(vm, &out))
}
