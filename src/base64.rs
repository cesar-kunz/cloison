//! `Uint8Array`'s base64 and hex (ECMA-262 §23.3.1, ES2026):
//! `Uint8Array.fromBase64` and `fromHex`, and `toBase64`, `toHex`,
//! `setFromBase64` and `setFromHex` on its prototype. Installed by
//! [`install`] (from `typed::install`).
//!
//! Decoding is the spec's FromBase64 and FromHex: what was decoded before
//! an error is still written by the `setFrom` methods, which report how
//! much they read and wrote.

use crate::interp::to_boolean;
use crate::object::{Class, Object, TypedKind, as_object, object_value};
use crate::props::is_object;
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, string_units};

const STANDARD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub(crate) fn install(vm: &mut Vm) {
    let k = TypedKind::U8 as usize;
    let (c, proto) = (vm.typed_constructors[k], vm.typed_prototypes[k]);
    vm.define_natives(c, &[("fromBase64", from_base64 as NativeFn, 1), ("fromHex", from_hex, 1)]);
    vm.define_natives(
        proto,
        &[
            ("toBase64", to_base64 as NativeFn, 0),
            ("toHex", to_hex, 0),
            ("setFromBase64", set_from_base64, 1),
            ("setFromHex", set_from_hex, 1),
        ],
    );
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// ValidateUint8Array: `v` a `Uint8Array` (in bounds or not).
fn uint8_array(v: Value) -> Result<*mut Object> {
    if crate::builtins::is_class(v, Class::TypedArray) && crate::typed::kind_of(as_object(v)) == TypedKind::U8 {
        Ok(as_object(v))
    } else {
        Err(Error::type_error("this is not a Uint8Array"))
    }
}

/// A `Uint8Array`'s bytes, if in bounds (GetUint8ArrayBytes).
fn bytes_of<'a>(o: *mut Object) -> Result<&'a mut [u8]> {
    match crate::typed::ta_length(o) {
        Some(n) => Ok(unsafe { std::slice::from_raw_parts_mut((*o).elems.cast::<u8>(), n) }),
        None => Err(Error::type_error("Cannot perform this operation on a detached or out of bounds typed array")),
    }
}

/// The string argument, or a TypeError.
fn string_arg<'a>(v: Value) -> Result<&'a [u16]> {
    string_units(v).ok_or_else(|| Error::type_error("the input is not a string"))
}

/// GetOptionsObject: an object, or none for `undefined`.
fn options(v: Value) -> Result<Option<Value>> {
    if v.is_undefined() {
        Ok(None)
    } else if is_object(v) {
        Ok(Some(v))
    } else {
        Err(Error::type_error("options is not an object"))
    }
}

/// Option `name` of `opts`: the index of its value in `allowed` (the first
/// if undefined), or a TypeError for anything else.
fn choice(vm: &mut Vm, opts: Option<Value>, name: &str, allowed: &[&str]) -> Result<usize> {
    let Some(o) = opts else { return Ok(0) };
    let a = vm.intern(name);
    let v = vm.get(o, a)?;
    if v.is_undefined() {
        return Ok(0);
    }
    let found = string_units(v).and_then(|u| allowed.iter().position(|s| s.encode_utf16().eq(u.iter().copied())));
    found.ok_or_else(|| Error::type_error(format!("{name} must be one of {}", allowed.join(", "))))
}

/// The alphabet option: whether `base64url`.
fn url_alphabet(vm: &mut Vm, opts: Option<Value>) -> Result<bool> {
    Ok(choice(vm, opts, "alphabet", &["base64", "base64url"])? == 1)
}

#[derive(Clone, Copy, PartialEq)]
enum LastChunk {
    Loose,
    Strict,
    StopBeforePartial,
}

fn last_chunk(vm: &mut Vm, opts: Option<Value>) -> Result<LastChunk> {
    Ok([LastChunk::Loose, LastChunk::Strict, LastChunk::StopBeforePartial]
        [choice(vm, opts, "lastChunkHandling", &["loose", "strict", "stop-before-partial"])?])
}

/// What a decoding read and made, and the error it stopped at, if any.
struct Decoded {
    read: usize,
    bytes: Vec<u8>,
    error: Option<Error>,
}

fn syntax_error(read: usize, bytes: Vec<u8>, what: &str) -> Decoded {
    Decoded { read, bytes, error: Some(Error::new_syntax(what)) }
}

fn is_ascii_whitespace(c: u16) -> bool {
    matches!(c, 0x09 | 0x0A | 0x0C | 0x0D | 0x20)
}

/// DecodeBase64Chunk: 2 to 4 characters' bytes; with `strict`, a partial
/// chunk's unused bits must be 0.
fn decode_chunk(chunk: &[u8], strict: bool) -> std::result::Result<Vec<u8>, ()> {
    let value = |c: u8| STANDARD.iter().position(|&s| s == c).unwrap() as u32;
    let mut n = 0u32;
    for i in 0..4 {
        n = n << 6 | chunk.get(i).map_or(0, |&c| value(c));
    }
    let b = [(n >> 16) as u8, (n >> 8) as u8, n as u8];
    let keep = chunk.len() - 1;
    if strict && b[keep..].iter().any(|&x| x != 0) {
        return Err(());
    }
    Ok(b[..keep].to_vec())
}

/// FromBase64: `s` decoded, at most `max` bytes of it.
fn decode_base64(s: &[u16], url: bool, last: LastChunk, max: usize) -> Decoded {
    let (mut read, mut bytes) = (0, Vec::new());
    if max == 0 {
        return Decoded { read, bytes, error: None };
    }
    let mut chunk: Vec<u8> = Vec::with_capacity(4);
    let skip = |mut i: usize| {
        while i < s.len() && is_ascii_whitespace(s[i]) {
            i += 1;
        }
        i
    };
    let mut i = 0;
    loop {
        i = skip(i);
        if i == s.len() {
            if !chunk.is_empty() {
                match last {
                    LastChunk::StopBeforePartial => return Decoded { read, bytes, error: None },
                    LastChunk::Loose if chunk.len() > 1 => bytes.extend(decode_chunk(&chunk, false).unwrap()),
                    _ => return syntax_error(read, bytes, "base64: incomplete last chunk"),
                }
            }
            return Decoded { read: s.len(), bytes, error: None };
        }
        let mut c = s[i];
        i += 1;
        if c == b'=' as u16 {
            if chunk.len() < 2 {
                return syntax_error(read, bytes, "base64: misplaced padding");
            }
            i = skip(i);
            if chunk.len() == 2 {
                if i == s.len() {
                    if last == LastChunk::StopBeforePartial {
                        return Decoded { read, bytes, error: None };
                    }
                    return syntax_error(read, bytes, "base64: incomplete padding");
                }
                if s[i] == b'=' as u16 {
                    i = skip(i + 1);
                }
            }
            if i < s.len() {
                return syntax_error(read, bytes, "base64: characters after padding");
            }
            return match decode_chunk(&chunk, last == LastChunk::Strict) {
                Ok(b) => {
                    bytes.extend(b);
                    Decoded { read: s.len(), bytes, error: None }
                }
                Err(()) => syntax_error(read, bytes, "base64: nonzero padding bits"),
            };
        }
        if url {
            if c == b'+' as u16 || c == b'/' as u16 {
                return syntax_error(read, bytes, "base64: not a base64url character");
            }
            c = match c {
                0x2D => b'+' as u16,
                0x5F => b'/' as u16,
                c => c,
            };
        }
        if c >= 0x80 || !STANDARD.contains(&(c as u8)) {
            return syntax_error(read, bytes, "base64: not a base64 character");
        }
        let remaining = max - bytes.len();
        if remaining == 1 && chunk.len() == 2 || remaining == 2 && chunk.len() == 3 {
            return Decoded { read, bytes, error: None };
        }
        chunk.push(c as u8);
        if chunk.len() == 4 {
            bytes.extend(decode_chunk(&chunk, false).unwrap());
            chunk.clear();
            read = i;
            if bytes.len() == max {
                return Decoded { read, bytes, error: None };
            }
        }
    }
}

/// FromHex: `s` decoded, at most `max` bytes of it.
fn decode_hex(s: &[u16], max: usize) -> Decoded {
    let mut bytes = Vec::new();
    if s.len() % 2 != 0 {
        return syntax_error(0, bytes, "hex: odd length");
    }
    let digit = |c: u16| char::from_u32(c as u32).and_then(|c| c.to_digit(16));
    let mut read = 0;
    while read < s.len() && bytes.len() < max {
        let (Some(h), Some(l)) = (digit(s[read]), digit(s[read + 1])) else {
            return syntax_error(read, bytes, "hex: not a hex digit");
        };
        read += 2;
        bytes.push((h << 4 | l) as u8);
    }
    Decoded { read, bytes, error: None }
}

/// A new `Uint8Array` of `bytes`.
fn new_uint8_array(vm: &mut Vm, bytes: &[u8]) -> Result<Value> {
    let ta = crate::typed::allocate(vm, TypedKind::U8, bytes.len())?;
    unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), (*as_object(ta)).elems.cast::<u8>(), bytes.len()) };
    Ok(ta)
}

fn from_base64(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let s = string_arg(arg(a, 0))?;
    let opts = options(arg(a, 1))?;
    let url = url_alphabet(vm, opts)?;
    let last = last_chunk(vm, opts)?;
    let d = decode_base64(s, url, last, usize::MAX);
    if let Some(e) = d.error {
        return Err(e);
    }
    new_uint8_array(vm, &d.bytes)
}

fn from_hex(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let d = decode_hex(string_arg(arg(a, 0))?, usize::MAX);
    if let Some(e) = d.error {
        return Err(e);
    }
    new_uint8_array(vm, &d.bytes)
}

/// What the `setFrom` methods do with a decoding into `o`: its bytes
/// written, then its error thrown, or `{ read, written }`.
fn written(vm: &mut Vm, o: *mut Object, d: Decoded) -> Result<Value> {
    // (A decoding runs no code: `o` is as it was checked.)
    let into = bytes_of(o)?;
    into[..d.bytes.len()].copy_from_slice(&d.bytes);
    if let Some(e) = d.error {
        return Err(e);
    }
    let r = object_value(vm.new_object(object_value(vm.object_prototype)));
    let (a_read, a_written) = (vm.intern("read"), vm.intern("written"));
    vm.create_data_property(r, Key::Name(a_read), Value::number(d.read as f64))?;
    vm.create_data_property(r, Key::Name(a_written), Value::number(d.bytes.len() as f64))?;
    Ok(r)
}

fn set_from_base64(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = uint8_array(this)?;
    crate::typed::writable_view(o)?;
    let s = string_arg(arg(a, 0))?;
    let opts = options(arg(a, 1))?;
    let url = url_alphabet(vm, opts)?;
    let last = last_chunk(vm, opts)?;
    let max = bytes_of(o)?.len();
    let d = decode_base64(s, url, last, max);
    written(vm, o, d)
}

fn set_from_hex(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = uint8_array(this)?;
    crate::typed::writable_view(o)?;
    let s = string_arg(arg(a, 0))?;
    let max = bytes_of(o)?.len();
    let d = decode_hex(s, max);
    written(vm, o, d)
}

fn to_base64(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = uint8_array(this)?;
    let opts = options(arg(a, 0))?;
    let alphabet = if url_alphabet(vm, opts)? { URL } else { STANDARD };
    let omit_padding = match opts {
        Some(o) => {
            let a = vm.intern("omitPadding");
            to_boolean(vm.get(o, a)?)
        }
        None => false,
    };
    let bytes = bytes_of(o)?;
    let mut out: Vec<u16> = Vec::with_capacity(bytes.len().div_ceil(3) * 4);
    for c in bytes.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..=c.len() {
            out.push(alphabet[(n >> (18 - 6 * k) & 63) as usize] as u16);
        }
        if !omit_padding {
            out.extend(std::iter::repeat_n(b'=' as u16, 3 - c.len()));
        }
    }
    Ok(vm.new_string(&out))
}

fn to_hex(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let bytes = bytes_of(uint8_array(this)?)?;
    let digits = b"0123456789abcdef";
    let out: Vec<u16> = bytes.iter().flat_map(|&b| [digits[(b >> 4) as usize] as u16, digits[(b & 15) as usize] as u16]).collect();
    Ok(vm.new_string(&out))
}
