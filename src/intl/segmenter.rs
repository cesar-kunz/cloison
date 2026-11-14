//! `Intl.Segmenter`, of graphemes, words and sentences. A segmenter's
//! internal slots are its locale (a string) and its granularity; a
//! Segments object's its string, the granularity and what `containing`
//! remembers (its last segment, marks); a Segment Iterator's its string,
//! the granularity and the index of its next segment. The rules are the same
//! in every locale, so all share the VM's one ICU4X segmenter of each
//! granularity and none refers to the segmenter it came from.

use super::icu::{Breaker, Granularity};
use super::{
    Kind, Root, canonicalize_locale_list, coerce_options, create, data_error, options_object, require, resolve_locale,
    root, slot, string_array, string_option, supported_locales,
};
use crate::object::{DONT_ENUM, Object, object_value};
use crate::temporal::arg;
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, string_units};

/// A segmenter's.
const LOCALE: usize = 1;
/// A Segments object's and a Segment Iterator's.
const STRING: usize = 1;
/// All three's: an index into [`GRANULARITIES`].
const GRANULARITY: usize = 2;
/// A Segments object's: where the segment `containing` last found
/// starts. A Segment Iterator's:
/// [[IteratedStringNextSegmentCodeUnitIndex]].
const AT: usize = 3;
/// A Segments object's: where the segment `containing` last found ends
/// (0 before any), and whether it is word-like.
const END: usize = 4;
const WORD_LIKE: usize = 5;
/// A Segments object's: `containing`'s marks, an array (the program
/// doesn't see) of boundaries it found, ascending, or undefined.
const MARKS: usize = 6;
/// How far apart `containing`'s marks are, at least, in code units.
const STRIDE: usize = 256;

const GRANULARITIES: [&str; 3] = ["grapheme", "word", "sentence"];
const WORD: usize = Granularity::Word as usize;

pub(super) fn install(vm: &mut Vm, intl: *mut Object) {
    let op = object_value(vm.object_prototype);
    let proto = vm.new_object(op);
    let c = vm.define_native(intl, "Segmenter", segmenter_ctor, 0, crate::jit::Ty::Any, true);
    crate::builtins::link_constructor(vm, c, proto);
    vm.define_natives(c, &[("supportedLocalesOf", supported_locales_of as NativeFn, 1)]);
    vm.define_natives(proto, &[("resolvedOptions", resolved_options as NativeFn, 0), ("segment", segment, 1)]);
    crate::symbol::to_string_tag(vm, proto, "Intl.Segmenter");
    vm.intl_roots[Root::SegmenterPrototype as usize] = proto;

    let segments = vm.new_object(op);
    vm.define_natives(segments, &[("containing", containing as NativeFn, 1)]);
    let iterator = vm.native_function("[Symbol.iterator]", segments_iterator, 0, crate::jit::Ty::Any, false);
    let a = vm.a_sym_iterator;
    vm.add_property_with(segments, a, object_value(iterator), DONT_ENUM);
    vm.intl_roots[Root::SegmentsPrototype as usize] = segments;

    let ip = object_value(vm.symbol_roots[crate::symbol::Root::IteratorPrototype as usize]);
    let iterators = vm.new_object(ip);
    vm.define_natives(iterators, &[("next", iterator_next as NativeFn, 0)]);
    crate::symbol::to_string_tag(vm, iterators, "Segmenter String Iterator");
    vm.intl_roots[Root::SegmentIteratorPrototype as usize] = iterators;
}

/// `new Intl.Segmenter(locales, options)`.
fn segmenter_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(Error::type_error("Constructor Intl.Segmenter requires 'new'"));
    }
    let f = vm.callee;
    let default = object_value(root(vm, Root::SegmenterPrototype));
    let proto = vm.take_new_target(f, default)?;
    // ResolveOptions (no relevant extension keys).
    let requested = canonicalize_locale_list(vm, arg(a, 0))?;
    let options = options_object(vm, arg(a, 1))?;
    string_option(vm, options, "localeMatcher", &["lookup", "best fit"])?;
    let r = resolve_locale(vm, &requested, &[])?;
    let granularity = string_option(vm, options, "granularity", &GRANULARITIES)?;
    let g = granularity.map_or(0, |g| GRANULARITIES.iter().position(|&n| n == g).unwrap());
    if vm.intl.breakers[g].is_none() {
        let granularity = [Granularity::Grapheme, Granularity::Word, Granularity::Sentence][g];
        vm.intl.breakers[g] = Some(Breaker::new(granularity).map_err(data_error)?);
    }
    let locale = crate::temporal::string(vm, &r.locale);
    Ok(create(vm, proto, Kind::Segmenter, &[locale, Value::from_i32(g as i32)]))
}

/// `Intl.Segmenter.supportedLocalesOf(locales, options)`.
fn supported_locales_of(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let requested = canonicalize_locale_list(vm, arg(a, 0))?;
    let options = coerce_options(vm, arg(a, 1))?;
    string_option(vm, options, "localeMatcher", &["lookup", "best fit"])?;
    let found = supported_locales(vm, &requested)?;
    string_array(vm, &found)
}

/// `Intl.Segmenter.prototype.resolvedOptions()`.
fn resolved_options(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = require(this, Kind::Segmenter, "Intl.Segmenter.prototype.resolvedOptions")?;
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    let a = vm.intern_name("locale");
    vm.add_property(r, a, slot(o, LOCALE));
    let granularity = crate::temporal::string(vm, GRANULARITIES[granularity(o)]);
    let a = vm.intern_name("granularity");
    vm.add_property(r, a, granularity);
    Ok(object_value(r))
}

/// `Intl.Segmenter.prototype.segment(string)`: a Segments object
/// (CreateSegmentsObject).
fn segment(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = require(this, Kind::Segmenter, "Intl.Segmenter.prototype.segment")?;
    let string = crate::intrinsics::to_string(vm, arg(a, 0))?;
    let proto = object_value(root(vm, Root::SegmentsPrototype));
    let zero = Value::number(0.0);
    Ok(create(vm, proto, Kind::Segments, &[string, slot(o, GRANULARITY), zero, zero, Value::FALSE, Value::UNDEFINED]))
}

/// `%IntlSegmentsPrototype%.containing(index)`: the segment the code
/// unit at `index` is in.
fn containing(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let o = require(this, Kind::Segments, "%IntlSegmentsPrototype%.containing")?;
    let n = crate::interp::to_number(arg(a, 0));
    if vm.conv_failed() {
        return Err(vm.take_conv_error());
    }
    let n = if n.is_nan() { 0.0 } else { n.trunc() };
    let units = string_units(slot(o, STRING)).unwrap_or(&[]);
    if n < 0.0 || n >= units.len() as f64 {
        return Ok(Value::UNDEFINED);
    }
    let n = n as usize;
    // FindBoundary before and after `n`. The segment the last call
    // found, if `n` is in it too (a long one would be scanned to its
    // end at each index).
    let (last, last_end) = (slot(o, AT).to_number() as usize, slot(o, END).to_number() as usize);
    if last <= n && n < last_end {
        return segment_data(vm, o, last, last_end, slot(o, WORD_LIKE) == Value::TRUE);
    }
    // Else the segments from a boundary known at or before `n`, up to
    // its own: the last segment's end if `n` is shortly after it (asking
    // index after index), else the last of the marks at or before `n`,
    // boundaries earlier calls passed, [`STRIDE`] apart or more, so that
    // no call scans from the string's start what another did (asking
    // the indices backwards, or here and there, in a long string).
    let mut marks = slot(o, MARKS);
    let count = if marks.is_undefined() { 0 } else { crate::far::array_length(crate::object::as_object(marks)) };
    let mark = |vm: &mut Vm, i: u32| Ok(vm.get_keyed(marks, Value::number(i as f64))?.to_number() as usize);
    let mut top = if count == 0 { 0 } else { mark(vm, count - 1)? };
    let mut start = if last_end <= n && n - last_end < STRIDE {
        last_end
    } else {
        // (The last mark at or before `n`; the string's start before the
        // first.)
        let (mut low, mut high) = (0, if n >= top { 0 } else { count });
        while low < high {
            let middle = (low + high) / 2;
            if mark(vm, middle)? <= n { low = middle + 1 } else { high = middle }
        }
        let known = if n >= top {
            top
        } else if low == 0 {
            0
        } else {
            mark(vm, low - 1)?
        };
        if last_end <= n { known.max(last_end) } else { known }
    };
    let mut found = Vec::new();
    let breaker = breaker(vm, o);
    let (end, word_like) = loop {
        if start >= top + STRIDE {
            found.push(Value::number(start as f64));
            top = start;
        }
        let (length, word_like) = breaker.first(&units[start..]);
        if start + length > n {
            break (start + length, word_like);
        }
        start += length;
    };
    if !found.is_empty() {
        if marks.is_undefined() {
            marks = vm.new_array();
            crate::promise::set_slot(o, MARKS, marks);
        }
        crate::vm::array_push(vm, marks, &found)?;
    }
    crate::promise::set_slot(o, AT, Value::number(start as f64));
    crate::promise::set_slot(o, END, Value::number(end as f64));
    crate::promise::set_slot(o, WORD_LIKE, Value::from_bool(word_like));
    segment_data(vm, o, start, end, word_like)
}

/// `%IntlSegmentsPrototype%[Symbol.iterator]()`: a Segment Iterator
/// (CreateSegmentIterator).
fn segments_iterator(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = require(this, Kind::Segments, "%IntlSegmentsPrototype%[Symbol.iterator]")?;
    let proto = object_value(root(vm, Root::SegmentIteratorPrototype));
    Ok(create(vm, proto, Kind::SegmentIterator, &[slot(o, STRING), slot(o, GRANULARITY), Value::number(0.0)]))
}

/// `%IntlSegmentIteratorPrototype%.next()`.
fn iterator_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = require(this, Kind::SegmentIterator, "%IntlSegmentIteratorPrototype%.next")?;
    let units = string_units(slot(o, STRING)).unwrap_or(&[]);
    let start = slot(o, AT).to_number() as usize;
    if start >= units.len() {
        return crate::symbol::iter_result(vm, Value::UNDEFINED, true);
    }
    // (A boundary found from the last one is the one found from the
    // string's start: no rule of UAX #29 looks back past a boundary.)
    let (length, word_like) = breaker(vm, o).first(&units[start..]);
    let end = start + length;
    crate::promise::set_slot(o, AT, Value::number(end as f64));
    let data = segment_data(vm, o, start, end, word_like)?;
    crate::symbol::iter_result(vm, data, false)
}

fn granularity(o: *mut Object) -> usize {
    slot(o, GRANULARITY).as_i32_unchecked() as usize
}

/// The ICU4X segmenter of `o`'s granularity (made when the segmenter
/// `o` is or comes from was).
fn breaker(vm: &Vm, o: *mut Object) -> &Breaker {
    vm.intl.breakers[granularity(o)].as_ref().unwrap()
}

/// CreateSegmentDataObject: `{segment, index, input}` of the segment
/// from `start` to `end` of the string of `o` (a Segments object or a
/// Segment Iterator), with `isWordLike` if the segments are words.
fn segment_data(vm: &mut Vm, o: *mut Object, start: usize, end: usize, word_like: bool) -> Result<Value> {
    let string = slot(o, STRING);
    let segment = crate::builtins::part(vm, string, string_units(string).unwrap_or(&[]), start, end);
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    let a = vm.intern_name("segment");
    vm.add_property(r, a, segment);
    let a = vm.intern_name("index");
    vm.add_property(r, a, Value::number(start as f64));
    let a = vm.intern_name("input");
    vm.add_property(r, a, slot(o, STRING));
    if granularity(o) == WORD {
        let a = vm.intern_name("isWordLike");
        vm.add_property(r, a, Value::from_bool(word_like));
    }
    Ok(object_value(r))
}
