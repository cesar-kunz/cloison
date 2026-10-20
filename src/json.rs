//! `JSON.stringify` (with `toJSON`, a replacer and a gap), `JSON.parse`
//! (with a reviver, which gets each primitive's source text), and
//! `JSON.rawJSON` and `isRawJSON`, as the spec has them. Installed by
//! [`install`].

use crate::interp::{call_function, check_native_stack, to_number, to_string_units};
use crate::object::{Class, DONT_ENUM, RAW_JSON, as_object, object_value};
use crate::props::{is_callable, is_object};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, string_units, wrapped};

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let json = vm.new_object(op);
    let a_json = vm.intern("JSON");
    vm.add_property_with(g, a_json, object_value(json), DONT_ENUM);
    vm.define_natives(
        json,
        &[
            ("stringify", json_stringify as NativeFn, 3),
            ("parse", json_parse, 2),
            ("rawJSON", json_raw_json, 1),
            ("isRawJSON", json_is_raw_json, 1),
        ],
    );
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

/// What one `stringify` call carries: the replacer, the property list,
/// the gap, and the objects being serialized (for cycles).
struct Stringify {
    replacer: Option<Value>,
    keys: Option<Vec<Value>>,
    gap: Vec<u16>,
    indent: Vec<u16>,
    stack: Vec<u64>,
    a_to_json: crate::object::Atom,
}

/// `JSON.stringify(value, replacer, space)`.
fn json_stringify(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let replacer = arg(a, 1);
    let mut st = Stringify {
        replacer: None,
        keys: None,
        gap: Vec::new(),
        indent: Vec::new(),
        stack: Vec::new(),
        a_to_json: vm.intern("toJSON"),
    };
    if is_callable(replacer) {
        st.replacer = Some(replacer);
    } else if crate::proxy::is_array(vm, replacer)? {
        let len = vm.length_of(replacer)?;
        let mut keys: Vec<Value> = Vec::new();
        let mut i = 0.0;
        while i < len {
            let v = vm.get_keyed(replacer, Value::number(i))?;
            let item = if string_units(v).is_some() {
                Some(v)
            } else if v.is_number() {
                let u = to_string_units(v);
                Some(vm.new_string(&u))
            } else if let Some(p) = wrapped(v)
                && (p.is_number() || string_units(p).is_some())
            {
                let u = to_string_units(v);
                Some(vm.new_string(&u))
            } else {
                None
            };
            if let Some(item) = item
                && !keys.iter().any(|&k| string_units(k) == string_units(item))
            {
                vm.handles.push(item);
                keys.push(item);
            }
            i += 1.0;
        }
        st.keys = Some(keys);
    }
    let mut space = arg(a, 2);
    if let Some(p) = wrapped(space) {
        if p.is_number() {
            space = Value::number(to_number(space));
        } else if string_units(p).is_some() {
            let u = to_string_units(space);
            space = vm.new_string(&u);
        }
    }
    if let Some(d) = space.as_number() {
        let n = d.clamp(0.0, 10.0) as usize;
        st.gap = vec![b' ' as u16; n];
    } else if let Some(u) = string_units(space) {
        st.gap = u[..u.len().min(10)].to_vec();
    }
    let depth = vm.handles.len();
    let r = vm.handle_scope(|vm| {
        let op = object_value(vm.object_prototype);
        let wrapper = object_value(vm.new_object(op));
        let empty = vm.new_string(&[]);
        let a_empty = vm.intern("");
        vm.create_data_property(wrapper, Key::Name(a_empty), arg(a, 0))?;
        let mut out = Vec::new();
        Ok(match property(vm, &mut st, empty, wrapper, &mut out)? {
            true => vm.new_string(&out),
            false => Value::UNDEFINED,
        })
    });
    vm.handles.truncate(depth);
    r
}

/// SerializeJSONProperty: `holder[key]` as JSON appended to `out`; false
/// if it has none (`undefined`, a function).
fn property(vm: &mut Vm, st: &mut Stringify, key: Value, holder: Value, out: &mut Vec<u16>) -> Result<bool> {
    check_native_stack(vm)?;
    let mut v = vm.get_keyed(holder, key)?;
    if is_object(v) || crate::bigint::is_bigint(v) {
        let f = vm.get(v, st.a_to_json)?;
        if is_callable(f) {
            v = call_function(vm, f, v, &[key])?;
        }
    }
    if let Some(r) = st.replacer {
        v = call_function(vm, r, holder, &[key, v])?;
    }
    if let Some(p) = wrapped(v) {
        if p.is_number() {
            v = Value::number(to_number(v));
        } else if string_units(p).is_some() {
            let u = to_string_units(v);
            v = vm.new_string(&u);
        } else if !crate::symbol::is_symbol(p) {
            // (A boolean's or a BigInt's; a symbol's stays an object.)
            v = p;
        }
    }
    if is_raw_json(v) {
        let a_raw = vm.intern("rawJSON");
        let raw = vm.get(v, a_raw)?;
        out.extend_from_slice(&to_string_units(raw));
        return Ok(true);
    }
    let push = |out: &mut Vec<u16>, s: &str| out.extend(s.encode_utf16());
    if v.is_null() {
        push(out, "null");
        return Ok(true);
    }
    if v.is_bool() {
        push(out, if v == Value::TRUE { "true" } else { "false" });
        return Ok(true);
    }
    if let Some(u) = string_units(v) {
        quote(u, out);
        return Ok(true);
    }
    if let Some(d) = v.as_number() {
        push(out, &if d.is_finite() { crate::vm::display(v) } else { "null".into() });
        return Ok(true);
    }
    if crate::bigint::is_bigint(v) {
        return Err(Error::type_error("Do not know how to serialize a BigInt"));
    }
    if !is_object(v) || is_callable(v) {
        return Ok(false);
    }
    if st.stack.contains(&v.to_bits()) {
        return Err(Error::type_error("Converting circular structure to JSON"));
    }
    st.stack.push(v.to_bits());
    vm.handles.push(v);
    let stepback = st.indent.clone();
    st.indent.extend_from_slice(&st.gap.clone());
    let o = as_object(v);
    let array = crate::proxy::is_array(vm, v)?;
    let mut parts: Vec<Vec<u16>> = Vec::new();
    if array {
        let len = vm.length_of(v)?;
        let mut i = 0.0;
        while i < len {
            let k = Value::number(i);
            let ks = vm.new_string(&to_string_units(k));
            let mut part = Vec::new();
            if !property(vm, st, ks, v, &mut part)? {
                part = "null".encode_utf16().collect();
            }
            parts.push(part);
            i += 1.0;
        }
    } else {
        let keys: Vec<Value> = match &st.keys {
            Some(k) => k.clone(),
            None if vm.proxies_exist && crate::proxy::is_proxy(vm, o) => {
                let mut ks = Vec::new();
                for key in crate::proxy::enumerable_own_keys(vm, o)? {
                    let s = vm.key_string(key);
                    vm.handles.push(s);
                    ks.push(s);
                }
                ks
            }
            None => {
                let mut ks = Vec::new();
                for (key, enumerable) in vm.own_keys(o) {
                    if enumerable {
                        let s = vm.key_string(key);
                        vm.handles.push(s);
                        ks.push(s);
                    }
                }
                ks
            }
        };
        for k in keys {
            let mut part = Vec::new();
            quote(string_units(k).unwrap(), &mut part);
            part.push(b':' as u16);
            if !st.gap.is_empty() {
                part.push(b' ' as u16);
            }
            if property(vm, st, k, v, &mut part)? {
                parts.push(part);
            }
        }
    }
    let (open, close) = if array { (b'[', b']') } else { (b'{', b'}') };
    out.push(open as u16);
    if !parts.is_empty() {
        let sep: Vec<u16> = if st.gap.is_empty() {
            vec![b',' as u16]
        } else {
            let mut s = vec![b',' as u16, b'\n' as u16];
            s.extend_from_slice(&st.indent);
            s
        };
        if !st.gap.is_empty() {
            out.push(b'\n' as u16);
            out.extend_from_slice(&st.indent);
        }
        for (i, p) in parts.iter().enumerate() {
            if i > 0 {
                out.extend_from_slice(&sep);
            }
            out.extend_from_slice(p);
        }
        if !st.gap.is_empty() {
            out.push(b'\n' as u16);
            out.extend_from_slice(&stepback);
        }
    }
    out.push(close as u16);
    st.stack.pop();
    st.indent = stepback;
    Ok(true)
}

/// QuoteJSONString.
fn quote(u: &[u16], out: &mut Vec<u16>) {
    out.push(b'"' as u16);
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        let esc = match c {
            0x22 => Some("\\\""),
            0x5C => Some("\\\\"),
            0x08 => Some("\\b"),
            0x0C => Some("\\f"),
            0x0A => Some("\\n"),
            0x0D => Some("\\r"),
            0x09 => Some("\\t"),
            _ => None,
        };
        match esc {
            Some(e) => out.extend(e.encode_utf16()),
            None if c < 0x20 => out.extend(format!("\\u{c:04x}").encode_utf16()),
            // A lone surrogate is escaped; a pair is kept.
            None if (0xD800..0xDC00).contains(&c) && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) => {
                out.push(c);
                out.push(u[i + 1]);
                i += 1;
            }
            None if (0xD800..0xE000).contains(&c) => out.extend(format!("\\u{c:04x}").encode_utf16()),
            None => out.push(c),
        }
        i += 1;
    }
    out.push(b'"' as u16);
}

/// `JSON.parse(text, reviver)`.
fn json_parse(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let u = to_string_units(arg(a, 0));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let reviver = arg(a, 1);
    if !is_callable(reviver) {
        return parse_text(vm, &u, false).map(|(v, _)| v);
    }
    vm.handle_scope(|vm| {
        let (v, record) = parse_text(vm, &u, true)?;
        let op = object_value(vm.object_prototype);
        let root = object_value(vm.new_object(op));
        vm.handles.push(root);
        let empty = vm.intern("");
        vm.create_data_property(root, Key::Name(empty), v)?;
        let key = vm.new_string(&[]);
        let a_source = vm.intern("source");
        internalize(vm, reviver, root, key, Some(&record), &u, a_source)
    })
}

/// `u` parsed as a whole JSON text (a JSON module's).
pub(crate) fn parse_json(vm: &mut Vm, u: &[u16]) -> Result<Value> {
    parse_text(vm, u, false).map(|(v, _)| v)
}

/// `u` parsed as a whole JSON text; with `track`, its values' records too
/// (each one rooted in `vm.handles` while the caller's scope lasts).
fn parse_text(vm: &mut Vm, u: &[u16], track: bool) -> Result<(Value, Record)> {
    let mut p = Parser { u, i: 0, track, records: Vec::new() };
    let v = p.value(vm)?;
    p.space();
    if p.i != u.len() {
        return Err(p.error());
    }
    let r = p.records.pop().unwrap_or(Record { value: v, kind: RecordKind::None });
    Ok((v, r))
}

/// A JSON Parse Record: the value a piece of the text made, and where the
/// text was (a primitive's source) or its elements' or entries' records.
struct Record {
    value: Value,
    kind: RecordKind,
}

enum RecordKind {
    /// Not tracked.
    None,
    /// A primitive, from `start` to `end` in the text.
    Source(usize, usize),
    Elements(Vec<Record>),
    /// By key; the last of a duplicated one.
    Entries(crate::jit::HashMap<Vec<u16>, Record>),
}

/// InternalizeJSONProperty: `holder[name]` revived, its properties first;
/// `record` what the parse made there, if anything: while `holder[name]`
/// is still that, the reviver gets a primitive's source text in its
/// third argument, `{ source }`.
#[allow(clippy::too_many_arguments)]
fn internalize(
    vm: &mut Vm,
    reviver: Value,
    holder: Value,
    name: Value,
    record: Option<&Record>,
    text: &[u16],
    a_source: crate::object::Atom,
) -> Result<Value> {
    check_native_stack(vm)?;
    let v = vm.get_keyed(holder, name)?;
    let record = record.filter(|r| crate::props::same_value(r.value, v));
    let context = object_value(vm.new_object(object_value(vm.object_prototype)));
    if let Some(&Record { kind: RecordKind::Source(start, end), .. }) = record
        && !is_object(v)
    {
        let source = vm.new_string(&text[start..end]);
        vm.create_data_property(context, Key::Name(a_source), source)?;
    }
    if is_object(v) {
        let o = as_object(v);
        let array = crate::proxy::is_array(vm, v)?;
        let keys: Vec<Key> = if array {
            let len = vm.length_of(v)?;
            (0..len as u32).map(Key::Index).collect()
        } else if crate::proxy::is_proxy(vm, o) {
            crate::proxy::enumerable_own_keys(vm, o)?
        } else {
            vm.own_keys(o).into_iter().filter(|&(_, e)| e).map(|(k, _)| k).collect()
        };
        for k in keys {
            let ks = vm.key_string(k);
            let child = match (record.map(|r| &r.kind), k) {
                (Some(RecordKind::Elements(es)), Key::Index(i)) if array => es.get(i as usize),
                (Some(RecordKind::Entries(es)), _) if !array => {
                    string_units(ks).and_then(|u| es.get(u))
                }
                _ => None,
            };
            let nv = internalize(vm, reviver, v, ks, child, text, a_source)?;
            // (A proxy's traps, and what they throw; a property that can't
            // be deleted or defined stays.)
            if nv.is_undefined() {
                crate::proxy::delete(vm, o, k)?;
            } else {
                let d = crate::props::Desc::data(nv, true, true, true);
                crate::proxy::define_property(vm, o, k, &d)?;
            }
        }
    }
    call_function(vm, reviver, holder, &[name, v, context])
}

/// Whether `v` is a `JSON.rawJSON` object.
fn is_raw_json(v: Value) -> bool {
    is_object(v) && unsafe { (*as_object(v)).class == Class::Plain && (*as_object(v)).flags & RAW_JSON != 0 }
}

/// `JSON.rawJSON(text)`: a frozen null-prototype object whose `rawJSON`
/// is `text`, which `stringify` writes as it is: a primitive's JSON text,
/// without whitespace around it.
fn json_raw_json(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let u = to_string_units(arg(a, 0));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let bad_end = |c: Option<&u16>| c.is_none_or(|&c| matches!(c, 0x09 | 0x0A | 0x0D | 0x20));
    if bad_end(u.first()) || bad_end(u.last()) || matches!(u[0], 0x5B | 0x7B) {
        return Err(Error::new_syntax("JSON.rawJSON: not a primitive's JSON text"));
    }
    parse_text(vm, &u, false)?;
    let o = vm.new_object(Value::NULL);
    let s = vm.new_string(&u);
    let a_raw = vm.intern("rawJSON");
    vm.create_data_property(object_value(o), Key::Name(a_raw), s)?;
    unsafe { (*o).flags |= RAW_JSON };
    vm.seal(o, true);
    Ok(object_value(o))
}

fn json_is_raw_json(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(is_raw_json(arg(a, 0))))
}

struct Parser<'a> {
    u: &'a [u16],
    i: usize,
    /// Whether to make each value's [`Record`]: each one parsed is pushed
    /// on `records`, an array's or object's taking its elements' or
    /// entries' off.
    track: bool,
    records: Vec<Record>,
}

impl Parser<'_> {
    fn error(&self) -> Error {
        Error::new_syntax(format!("Unexpected token in JSON at position {}", self.i))
    }

    fn space(&mut self) {
        while self.i < self.u.len() && matches!(self.u[self.i], 0x20 | 0x09 | 0x0A | 0x0D) {
            self.i += 1;
        }
    }

    fn peek(&self) -> Option<u16> {
        self.u.get(self.i).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        self.space();
        if self.peek() == Some(c as u16) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn word(&mut self, w: &str, v: Value) -> Result<Value> {
        let w: Vec<u16> = w.encode_utf16().collect();
        if self.u[self.i..].starts_with(&w) {
            self.i += w.len();
            Ok(v)
        } else {
            Err(self.error())
        }
    }

    /// A value; if tracking, its record pushed (its value rooted).
    fn value(&mut self, vm: &mut Vm) -> Result<Value> {
        check_native_stack(vm)?;
        self.space();
        let start = self.i;
        let base = self.records.len();
        let mut keys: Vec<Vec<u16>> = Vec::new();
        // (`container`: whether an object's, or an array's.)
        let (v, container) = match self.peek().map(|c| c as u8 as char) {
            Some('{') if self.peek() < Some(128) => {
                self.i += 1;
                let o = object_value(vm.new_object(object_value(vm.object_prototype)));
                if !self.eat(b'}') {
                    loop {
                        self.space();
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return Err(self.error());
                        }
                        let v = self.value(vm)?;
                        let key = vm.new_string(&k);
                        let key = vm.to_key(key);
                        vm.create_data_property(o, key, v)?;
                        if self.track {
                            keys.push(k);
                        }
                        if self.eat(b'}') {
                            break;
                        }
                        if !self.eat(b',') {
                            return Err(self.error());
                        }
                    }
                }
                (o, Some(true))
            }
            Some('[') if self.peek() < Some(128) => {
                self.i += 1;
                let arr = vm.new_array();
                let mut n = 0.0;
                if !self.eat(b']') {
                    loop {
                        let v = self.value(vm)?;
                        vm.create_data_property(arr, Key::Index(n as u32), v)?;
                        n += 1.0;
                        if self.eat(b']') {
                            break;
                        }
                        if !self.eat(b',') {
                            return Err(self.error());
                        }
                    }
                }
                (arr, Some(false))
            }
            Some('"') if self.peek() < Some(128) => {
                let s = self.string()?;
                (vm.new_string(&s), None)
            }
            Some('t') if self.peek() < Some(128) => (self.word("true", Value::TRUE)?, None),
            Some('f') if self.peek() < Some(128) => (self.word("false", Value::FALSE)?, None),
            Some('n') if self.peek() < Some(128) => (self.word("null", Value::NULL)?, None),
            Some('-' | '0'..='9') if self.peek() < Some(128) => (self.number()?, None),
            _ => return Err(self.error()),
        };
        if self.track {
            if v.is_cell() {
                vm.handles.push(v);
            }
            let kind = match container {
                None => RecordKind::Source(start, self.i),
                Some(true) => RecordKind::Entries(keys.into_iter().zip(self.records.split_off(base)).collect()),
                Some(false) => RecordKind::Elements(self.records.split_off(base)),
            };
            self.records.push(Record { value: v, kind });
        }
        Ok(v)
    }

    /// A JSON number: `-? int frac? exp?`, `int` without leading zeros.
    fn number(&mut self) -> Result<Value> {
        let start = self.i;
        let at = |p: &Self| p.u.get(p.i).copied().map(|c| c as u8 as char).filter(|_| p.u[p.i] < 128);
        if at(self) == Some('-') {
            self.i += 1;
        }
        match at(self) {
            Some('0') => self.i += 1,
            Some('1'..='9') => {
                while matches!(at(self), Some('0'..='9')) {
                    self.i += 1;
                }
            }
            _ => return Err(self.error()),
        }
        if at(self) == Some('.') {
            self.i += 1;
            if !matches!(at(self), Some('0'..='9')) {
                return Err(self.error());
            }
            while matches!(at(self), Some('0'..='9')) {
                self.i += 1;
            }
        }
        if matches!(at(self), Some('e' | 'E')) {
            self.i += 1;
            if matches!(at(self), Some('+' | '-')) {
                self.i += 1;
            }
            if !matches!(at(self), Some('0'..='9')) {
                return Err(self.error());
            }
            while matches!(at(self), Some('0'..='9')) {
                self.i += 1;
            }
        }
        let s = String::from_utf16_lossy(&self.u[start..self.i]);
        s.parse::<f64>().map(Value::number).map_err(|_| self.error())
    }

    fn string(&mut self) -> Result<Vec<u16>> {
        if self.peek() != Some(b'"' as u16) {
            return Err(self.error());
        }
        self.i += 1;
        let mut out = Vec::new();
        loop {
            let Some(c) = self.peek() else { return Err(self.error()) };
            self.i += 1;
            match c {
                0x22 => return Ok(out),
                0x5C => {
                    let Some(e) = self.peek() else { return Err(self.error()) };
                    self.i += 1;
                    out.push(match e as u8 {
                        b'"' if e < 128 => 0x22,
                        b'\\' if e < 128 => 0x5C,
                        b'/' if e < 128 => 0x2F,
                        b'b' if e < 128 => 0x08,
                        b'f' if e < 128 => 0x0C,
                        b'n' if e < 128 => 0x0A,
                        b'r' if e < 128 => 0x0D,
                        b't' if e < 128 => 0x09,
                        b'u' if e < 128 => {
                            let h = self.u.get(self.i..self.i + 4).ok_or_else(|| self.error())?;
                            if !h.iter().all(|&c| c < 128 && (c as u8).is_ascii_hexdigit()) {
                                return Err(self.error());
                            }
                            let s = String::from_utf16_lossy(h);
                            self.i += 4;
                            u16::from_str_radix(&s, 16).map_err(|_| self.error())?
                        }
                        _ => return Err(self.error()),
                    });
                }
                _ if c < 0x20 => return Err(self.error()),
                _ => out.push(c),
            }
        }
    }
}
