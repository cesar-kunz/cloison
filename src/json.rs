//! `JSON.stringify` (with `toJSON`, a replacer and a gap), `JSON.parse`
//! (with a reviver, which gets each primitive's source text), and
//! `JSON.rawJSON` and `isRawJSON`, as the spec has them. Installed by
//! [`install`].

use crate::interp::{call_function, check_native_stack, to_number, to_string_units};
use crate::object::{ACCESSOR, Atom, Class, DONT_ENUM, Obj, RAW_JSON, object_value};
use crate::props::{is_callable, is_object};
use crate::raw::{Live, ObjectPtr};
use crate::value::Value;
use crate::vm::{Error, Key, NativeFn, Result, Vm, check_string_len, is_string, string_len, string_units, wrapped};

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global_obj();
    let op = object_value(vm.object_prototype);
    let json = vm.new_plain(op);
    let a_json = vm.intern_name("JSON");
    vm.add_property_with(g, a_json, object_value(json.ptr()), DONT_ENUM);
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
    /// The indentation: so many gaps as objects are being serialized.
    indent: Vec<u16>,
    stack: Vec<u64>,
    a_to_json: crate::object::Atom,
}

/// `JSON.stringify(value, replacer, space)`.
fn json_stringify(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    // (No cell moves meanwhile: what it serializes or revives is held in
    // Rust structures, objects by address, see `Vm::unmovable`.)
    vm.unmovable += 1;
    let r = json_stringify_(vm, this, a);
    vm.unmovable -= 1;
    r
}

fn json_stringify_(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    // (The replacer's keys rooted in `vm.handles` from the first: left on
    // every return, errors' too, or a loop of calls would pile them up
    // until the run ends.)
    let depth = vm.handles.len();
    let r = stringify_rooted(vm, a);
    vm.handles.truncate(depth);
    r
}

/// [`json_stringify_`], what it roots left to it.
fn stringify_rooted(vm: &mut Vm, a: &[Value]) -> Result<Value> {
    let replacer = arg(a, 1);
    let mut st = Stringify {
        replacer: None,
        keys: None,
        gap: Vec::new(),
        indent: Vec::new(),
        stack: Vec::new(),
        a_to_json: vm.intern_name("toJSON"),
    };
    if is_callable(replacer) {
        st.replacer = Some(replacer);
    } else if crate::proxy::is_array(vm, replacer)? {
        let len = vm.length_of(replacer)?;
        let mut keys: Vec<Value> = Vec::new();
        let mut i = 0.0;
        while i < len {
            let v = vm.get_keyed(replacer, Value::number(i))?;
            let item = if is_string(v) {
                Some(v)
            } else if v.is_number() {
                let u = to_string_units(v);
                Some(vm.new_string(&u))
            } else if let Some(p) = wrapped(v)
                && (p.is_number() || is_string(p))
            {
                // (Its exception stops the operation: no next entry read.)
                let u = to_string_units(v);
                if vm.conv_failed() {
                    return Err(vm.take_conv_error());
                }
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
        } else if is_string(p) {
            let u = to_string_units(space);
            space = vm.new_string(&u);
        }
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
    }
    if let Some(d) = space.as_number() {
        let n = d.clamp(0.0, 10.0) as usize;
        st.gap = vec![b' ' as u16; n];
    } else if let Some(u) = string_units(space) {
        st.gap = u[..u.len().min(10)].to_vec();
    }
    vm.handle_scope(|vm| {
        let op = object_value(vm.object_prototype);
        let wrapper = object_value(vm.new_plain(op).ptr());
        let empty = vm.new_string(&[]);
        // (The holder and key a `toJSON` or replacer gets: rooted.)
        vm.handles.extend([wrapper, empty]);
        let a_empty = vm.intern_name("");
        vm.create_data_property(wrapper, Key::Name(a_empty), arg(a, 0))?;
        let mut out = Vec::new();
        Ok(match property(vm, &mut st, empty, wrapper, &mut out)? {
            true => vm.new_string(&out),
            false => Value::UNDEFINED,
        })
    })
}

/// A property's key while it is serialized: its string is made only for a
/// `toJSON` or a replacer to get.
#[derive(Clone, Copy)]
enum PKey {
    Str(Value),
    /// An array's index (an array-like's may be past an element's).
    Num(f64),
    Name(Atom),
}

impl PKey {
    fn string(self, vm: &mut Vm) -> Value {
        match self {
            PKey::Str(s) => s,
            PKey::Num(i) => vm.new_string(&to_string_units(Value::number(i))),
            PKey::Name(a) => vm.atom_string(a),
        }
    }

    fn get(self, vm: &mut Vm, holder: Value) -> Result<Value> {
        match self {
            PKey::Str(s) => vm.get_keyed(holder, s),
            PKey::Num(i) => vm.get_keyed(holder, Value::number(i)),
            PKey::Name(a) => vm.get(holder, a),
        }
    }
}

fn push_ascii(out: &mut Vec<u16>, s: &str) {
    out.extend(s.bytes().map(u16::from));
}

/// SerializeJSONProperty: `holder[key]` as JSON appended to `out`; false
/// if it has none (`undefined`, a function).
fn property(vm: &mut Vm, st: &mut Stringify, key: Value, holder: Value, out: &mut Vec<u16>) -> Result<bool> {
    let v = vm.get_keyed(holder, key)?;
    value(vm, st, PKey::Str(key), holder, v, out)
}

/// [`property`] from where `holder[key]` was read: `v`. An array's
/// elements and a plain object's data properties are read where they
/// are, and everything is written to `out` as it comes (a 1 MiB array of
/// small integers made a key string, a part and its copy for each: 19
/// times V8's time).
fn value(vm: &mut Vm, st: &mut Stringify, key: PKey, holder: Value, mut v: Value, out: &mut Vec<u16>) -> Result<bool> {
    if v.is_int32() {
        if let Some(r) = st.replacer {
            let k = key.string(vm);
            v = call_function(vm, r, holder, &[k, v])?;
        }
        if v.is_int32() {
            let mut b = [0u8; 11];
            let mut n = v.as_i32_unchecked() as i64;
            let neg = n < 0;
            n = n.abs();
            let mut i = b.len();
            loop {
                i -= 1;
                b[i] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            if neg {
                i -= 1;
                b[i] = b'-';
            }
            out.extend(b[i..].iter().map(|&c| c as u16));
            return Ok(true);
        }
    } else {
        check_native_stack(vm)?;
        if is_object(v) || crate::bigint::is_bigint(v) {
            // (Maybe a getter's new object, the receiver of a proxy's
            // trap on its prototype chain: rooted across the trap's
            // lookup.)
            let a = st.a_to_json;
            let f = vm.rooted(&[v], |vm| vm.get(v, a))?;
            if is_callable(f) {
                let k = key.string(vm);
                v = call_function(vm, f, v, &[k])?;
            }
        }
        if let Some(r) = st.replacer {
            let k = key.string(vm);
            v = call_function(vm, r, holder, &[k, v])?;
        }
    }
    if let Some(p) = wrapped(v) {
        if p.is_number() {
            v = Value::number(to_number(v));
        } else if is_string(p) {
            let u = to_string_units(v);
            v = vm.new_string(&u);
        } else if !crate::symbol::is_symbol(p) {
            // (A boolean's or a BigInt's; a symbol's stays an object.)
            v = p;
        }
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
    }
    if is_raw_json(v) {
        let a_raw = vm.intern_name("rawJSON");
        let raw = vm.get(v, a_raw)?;
        // (Past the longest string: thrown before a long text is read.)
        check_string_len(out.len() + string_len(raw).unwrap_or(0))?;
        out.extend_from_slice(&to_string_units(raw));
        if vm.conv_failed() {
            return Err(vm.take_conv_error());
        }
        check_string_len(out.len())?;
        return Ok(true);
    }
    if v.is_null() {
        push_ascii(out, "null");
        return Ok(true);
    }
    if v.is_bool() {
        push_ascii(out, if v == Value::TRUE { "true" } else { "false" });
        return Ok(true);
    }
    if let Some(n) = string_len(v) {
        // (Past the longest string: thrown before a long string's units are
        // read.)
        check_string_len(out.len() + n + 2)?;
        quote(string_units(v).unwrap(), out)?;
        return Ok(true);
    }
    if let Some(d) = v.as_number() {
        if v.is_int32() {
            push_ascii(out, &crate::vm::display_raw(v));
        } else if d.is_finite() {
            push_ascii(out, crate::vm::number_ascii(d).as_str());
        } else {
            push_ascii(out, "null");
        }
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
    let stepback = st.indent.len();
    let gap = !st.gap.is_empty();
    if gap {
        let Stringify { indent, gap, .. } = st;
        indent.extend_from_slice(gap);
    }
    let o = Obj::of(v);
    let array = crate::proxy::is_array(vm, v)?;
    // The units `out` gets after a part: the closing bracket, with a gap
    // after a newline and the indentation. `out` with them is checked
    // against the longest string after each part.
    let tail = if gap { 2 + stepback } else { 1 };
    // Before a part: `,` after another, and with a gap a newline and the
    // indentation.
    let lead = |st: &Stringify, out: &mut Vec<u16>, any: bool| {
        if any {
            out.push(b',' as u16);
        }
        if gap {
            out.push(b'\n' as u16);
            out.extend_from_slice(&st.indent);
        }
    };
    let mut any = false;
    out.push(if array { b'[' } else { b'{' } as u16);
    if array {
        let len = vm.length_of(v)?;
        let mut i = 0.0;
        while i < len {
            lead(st, out, any);
            // (An array's element there, not a hole: no code runs.)
            let own = {
                (o.class() == Class::Array && i < o.elems_len() as f64)
                    .then(|| o.elem(i as usize))
                    .filter(|&e| e != Value::EMPTY)
            };
            let e = match own {
                Some(e) => e,
                None => vm.get_keyed(v, Value::number(i))?,
            };
            if !value(vm, st, PKey::Num(i), v, e, out)? {
                push_ascii(out, "null");
            }
            any = true;
            check_string_len(out.len() + tail)?;
            i += 1.0;
        }
    } else {
        // (With its slot in the shape, of a plain object's name.)
        let keys: Vec<(PKey, u32)> = match &st.keys {
            Some(k) => k.iter().map(|&k| (PKey::Str(k), u32::MAX)).collect(),
            None if vm.proxies_exist && crate::proxy::is_proxy(vm, o) => {
                let mut ks = Vec::new();
                for key in crate::proxy::enumerable_own_keys(vm, o)? {
                    let s = vm.key_string(key);
                    vm.handles.push(s);
                    ks.push((PKey::Str(s), u32::MAX));
                }
                ks
            }
            None if vm.plain_named(o) => {
                let s = o.shape().live();
                let slots = vm.listed_slots(o, false, false).unwrap_or_default();
                slots
                    .into_iter()
                    .map(|k| {
                        let name = s.keys()[k as usize];
                        // (While a `toJSON` deletes it and collects.)
                        vm.hold_atom(name);
                        (PKey::Name(name), k)
                    })
                    .collect()
            }
            None => {
                let mut ks = Vec::new();
                for (key, enumerable) in vm.own_keys(o) {
                    if enumerable {
                        ks.push(match key {
                            Key::Index(i) => (PKey::Num(i as f64), u32::MAX),
                            Key::Name(a) => {
                                vm.hold_atom(a);
                                (PKey::Name(a), u32::MAX)
                            }
                        });
                    }
                }
                ks
            }
        };
        for (k, slot) in keys {
            let mark = out.len();
            lead(st, out, any);
            match k {
                PKey::Str(s) => quote(string_units(s).unwrap(), out)?,
                PKey::Num(i) => {
                    out.push(b'"' as u16);
                    push_ascii(out, &crate::vm::display_raw(Value::number(i)));
                    out.push(b'"' as u16);
                }
                PKey::Name(a) => {
                    let name = vm.atom_name(a);
                    if name.bytes().all(|b| (0x20..0x80).contains(&b) && b != b'"' && b != b'\\') {
                        out.push(b'"' as u16);
                        push_ascii(out, name);
                        out.push(b'"' as u16);
                    } else {
                        // (Its lone surrogates mapped back: see `property_key`.)
                        let mut u = Vec::new();
                        crate::js::lex::string_units(name, &mut u);
                        quote(&u, out)?;
                    }
                }
            }
            out.push(b':' as u16);
            if gap {
                out.push(b' ' as u16);
            }
            // (The data property that is still at the name's slot.)
            let own = match k {
                PKey::Name(a) => {
                    let s = o.shape().live();
                    (s.keys().get(slot as usize) == Some(&a) && s.attrs()[slot as usize] & ACCESSOR == 0)
                        .then(|| o.slot_value(s.location(slot)))
                }
                _ => None,
            };
            let pv = match own {
                Some(pv) => pv,
                None => k.get(vm, v)?,
            };
            if value(vm, st, k, v, pv, out)? {
                any = true;
                check_string_len(out.len() + tail)?;
            } else {
                out.truncate(mark);
            }
        }
    }
    st.indent.truncate(stepback);
    if any && gap {
        out.push(b'\n' as u16);
        out.extend_from_slice(&st.indent);
    }
    out.push(if array { b']' } else { b'}' } as u16);
    st.stack.pop();
    Ok(true)
}

/// QuoteJSONString; a RangeError past the longest string (an escape is
/// up to 6 units of one: the escaped length counted first if it could
/// be past, before anything is written).
fn quote(u: &[u16], out: &mut Vec<u16>) -> Result<()> {
    if out.len() + 6 * u.len() + 2 > crate::vm::MAX_STRING_LEN {
        check_string_len(out.len() + quoted_len(u))?;
    }
    let hex = |out: &mut Vec<u16>, c: u16| {
        push_ascii(out, "\\u");
        out.extend((0..4).rev().map(|d| b"0123456789abcdef"[(c >> (4 * d)) as usize & 15] as u16));
    };
    // (What is written as it is.)
    let plain = |c: u16| c >= 0x20 && c != 0x22 && c != 0x5C && !(0xD800..0xE000).contains(&c);
    out.reserve(u.len() + 2);
    out.push(b'"' as u16);
    let mut i = 0;
    while i < u.len() {
        let run = u[i..].iter().position(|&c| !plain(c)).unwrap_or(u.len() - i);
        out.extend_from_slice(&u[i..i + run]);
        i += run;
        let Some(&c) = u.get(i) else { break };
        match c {
            0x22 => push_ascii(out, "\\\""),
            0x5C => push_ascii(out, "\\\\"),
            0x08 => push_ascii(out, "\\b"),
            0x0C => push_ascii(out, "\\f"),
            0x0A => push_ascii(out, "\\n"),
            0x0D => push_ascii(out, "\\r"),
            0x09 => push_ascii(out, "\\t"),
            _ if c < 0x20 => hex(out, c),
            // A lone surrogate is escaped; a pair is kept.
            _ if c < 0xDC00 && i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) => {
                out.push(c);
                out.push(u[i + 1]);
                i += 1;
            }
            _ => hex(out, c),
        }
        i += 1;
    }
    out.push(b'"' as u16);
    Ok(())
}

/// The units [`quote`] writes for `u`.
fn quoted_len(u: &[u16]) -> usize {
    let mut n = 2;
    let mut i = 0;
    while i < u.len() {
        let c = u[i];
        n += match c {
            0x22 | 0x5C | 0x08 | 0x0C | 0x0A | 0x0D | 0x09 => 2,
            _ if c < 0x20 => 6,
            0xD800..0xDC00 if i + 1 < u.len() && (0xDC00..0xE000).contains(&u[i + 1]) => {
                i += 1;
                2
            }
            0xD800..0xE000 => 6,
            _ => 1,
        };
        i += 1;
    }
    n
}

/// `JSON.parse(text, reviver)`.
fn json_parse(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    // (No cell moves meanwhile: what it serializes or revives is held in
    // Rust structures, objects by address, see `Vm::unmovable`.)
    vm.unmovable += 1;
    let r = json_parse_(vm, this, a);
    vm.unmovable -= 1;
    r
}

fn json_parse_(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
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
        let root = object_value(vm.new_plain(op).ptr());
        vm.handles.push(root);
        let empty = vm.intern_name("");
        vm.create_data_property(root, Key::Name(empty), v)?;
        let key = vm.new_string(&[]);
        vm.handles.push(key);
        let a_source = vm.intern_name("source");
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
    let context = object_value((vm.new_plain(object_value(vm.object_prototype))).ptr());
    // (Both held across the reviver's calls below: rooted.)
    vm.rooted(&[v, context], |vm| internalize_value(vm, reviver, holder, name, v, context, record, text, a_source))
}

/// [`internalize`] of `v`, `holder[name]` as read, with its `context`.
#[allow(clippy::too_many_arguments)]
fn internalize_value(
    vm: &mut Vm,
    reviver: Value,
    holder: Value,
    name: Value,
    v: Value,
    context: Value,
    record: Option<&Record>,
    text: &[u16],
    a_source: crate::object::Atom,
) -> Result<Value> {
    let record = record.filter(|r| crate::props::same_value(r.value, v));
    if let Some(&Record { kind: RecordKind::Source(start, end), .. }) = record
        && !is_object(v)
    {
        let source = vm.new_string(&text[start..end]);
        vm.create_data_property(context, Key::Name(a_source), source)?;
    }
    if is_object(v) {
        let o = Obj::of(v);
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
                (Some(RecordKind::Entries(es)), _) if !array => string_units(ks).and_then(|u| es.get(u)),
                _ => None,
            };
            let nv = vm.rooted(&[ks], |vm| internalize(vm, reviver, v, ks, child, text, a_source))?;
            // (A proxy's traps, and what they throw; a property that can't
            // be deleted or defined stays.)
            if nv.is_undefined() {
                crate::proxy::delete(vm, o, k)?;
            } else {
                let d = crate::props::Desc::data(nv, true, true, true);
                // (The reviver's new value maybe, held in `d` across a
                // proxy's trap lookup: rooted.)
                vm.rooted(&[nv], |vm| crate::proxy::define_property(vm, o, k, &d))?;
            }
        }
    }
    call_function(vm, reviver, holder, &[name, v, context])
}

/// Whether `v` is a `JSON.rawJSON` object.
fn is_raw_json(v: Value) -> bool {
    // (A mapped `arguments` object's `MAPPED` is the bit too: it is never
    // frozen.)
    let both = RAW_JSON | crate::object::FROZEN;
    is_object(v) && (Obj::of(v).class() == Class::Plain && Obj::of(v).flags() & both == both)
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
    let o = vm.new_plain(Value::NULL);
    let s = vm.new_string(&u);
    let a_raw = vm.intern_name("rawJSON");
    vm.create_data_property(object_value(o.ptr()), Key::Name(a_raw), s)?;
    unsafe { (*o.ptr()).flags |= RAW_JSON };
    vm.seal(o, true);
    Ok(object_value(o.ptr()))
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

/// A property's name in the text as a key (`Vm::to_key` of its string).
fn property_key(vm: &mut Vm, k: &[u16]) -> Key {
    if let Some(i) = crate::vm::canonical_index(k) {
        return Key::Index(i);
    }
    let mut b = [0u8; 64];
    if k.len() <= b.len() && k.iter().all(|&c| c < 0x80) {
        k.iter().zip(&mut b).for_each(|(&c, b)| *b = c as u8);
        return Key::Name(vm.intern_name(std::str::from_utf8(&b[..k.len()]).unwrap_or("")));
    }
    Key::Name(vm.intern_name(&crate::js::lex::units_to_string(k)))
}

impl<'a> Parser<'a> {
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
        let rest = &self.u[self.i..];
        if rest.len() >= w.len() && w.bytes().zip(rest).all(|(b, &c)| b as u16 == c) {
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
                let o = object_value((vm.new_plain(object_value(vm.object_prototype))).ptr());
                if !self.eat(b'}') {
                    loop {
                        self.space();
                        let k = self.string()?;
                        if !self.eat(b':') {
                            return Err(self.error());
                        }
                        let v = self.value(vm)?;
                        // (A name the object has, stored in its slot, a
                        // new one added: no string of it, no descriptor.)
                        match property_key(vm, &k) {
                            Key::Name(a) => {
                                let ob = Obj::of(o);
                                match ob.shape().live().lookup(a) {
                                    Some(slot) => vm.store_field(ob, slot, v),
                                    None => {
                                        vm.add_property_keyed(ob, a, v);
                                    }
                                }
                            }
                            key => vm.create_data_property(o, key, v)?,
                        }
                        if self.track {
                            keys.push(k.into_owned());
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
                let ao = Obj::of(arr);
                let mut n = 0;
                if !self.eat(b']') {
                    loop {
                        let v = self.value(vm)?;
                        // (Pushed: traced from here on.)
                        vm.reserve_elems(ao, n + 1);
                        vm.store_element(ao, n, v);
                        n += 1;
                        // SAFETY: within the capacity `reserve_elems` made above.
                        unsafe { ao.set_elems_len(n as u32) };
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
        let t = &self.u[start..self.i];
        // (A short integer: its digits' value.)
        if t.len() <= 9 && t.iter().all(|&c| c != b'.' as u16 && c | 0x20 != b'e' as u16) {
            let neg = t[0] == b'-' as u16;
            let n = t[neg as usize..].iter().fold(0i32, |n, &c| n * 10 + (c - b'0' as u16) as i32);
            return Ok(if !neg {
                Value::from_i32(n)
            } else if n == 0 {
                Value::number(-0.0)
            } else {
                Value::from_i32(-n)
            });
        }
        let mut b = [0u8; 64];
        let parsed = if t.len() <= b.len() {
            t.iter().zip(&mut b).for_each(|(&c, b)| *b = c as u8);
            std::str::from_utf8(&b[..t.len()]).ok().and_then(|s| s.parse::<f64>().ok())
        } else {
            String::from_utf16_lossy(t).parse::<f64>().ok()
        };
        parsed.map(Value::number).ok_or_else(|| self.error())
    }

    /// A string's units: the text's own where it has no escape.
    fn string(&mut self) -> Result<std::borrow::Cow<'a, [u16]>> {
        if self.peek() != Some(b'"' as u16) {
            return Err(self.error());
        }
        self.i += 1;
        let u = self.u;
        let run = u[self.i..].iter().position(|&c| c == 0x22 || c == 0x5C || c < 0x20).unwrap_or(u.len() - self.i);
        let plain = &u[self.i..self.i + run];
        self.i += run;
        if self.peek() == Some(0x22) {
            self.i += 1;
            return Ok(plain.into());
        }
        let mut out = plain.to_vec();
        loop {
            let Some(c) = self.peek() else { return Err(self.error()) };
            self.i += 1;
            match c {
                0x22 => return Ok(out.into()),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn handles(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
        Ok(Value::from_i32(vm.handles.len() as i32))
    }

    /// A replacer's keys, rooted while it runs, are left when it returns,
    /// with its result or its error: none piles up in `vm.handles` over a
    /// run's calls.
    #[test]
    fn stringify_leaves_no_handle() {
        let mut vm = Vm::new();
        let g = Obj::of(object_value(vm.global));
        vm.define_natives(g, &[("handles", handles, 0)]);
        let m = crate::js::compile(
            r#"var n = new Number(2); n.valueOf = function () { throw 1; };
            var x = { a: 1, b: 2 }, before = handles();
            for (var i = 0; i < 100; i++) {
                JSON.stringify(x, ["a", "b", 3, new String("c")]);
                try { JSON.stringify(x, ["a", "b"], n); } catch (e) {}
            }
            handles() - before"#,
        )
        .unwrap();
        assert_eq!(vm.run_raw(&m).map(|v| v.as_number()), Ok(Some(0.0)));
    }
}
