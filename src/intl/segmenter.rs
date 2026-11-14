//! `Intl.Segmenter`, of graphemes alone: `word` and `sentence` are
//! refused (the data has no rules for them). A segmenter's internal slot
//! is its locale (a string); a Segments object's its string; a Segment
//! Iterator's its string and the index of its next segment. Graphemes
//! break the same in every locale, so the three share the VM's one ICU4X
//! segmenter and none refers to the segmenter it came from.

use super::{
    Kind, Root, canonicalize_locale_list, coerce_options, create, data_error, icu, options_object, require,
    resolve_locale, root, slot, string_array, string_option, supported_locales,
};
use crate::object::{DONT_ENUM, Object, object_value};
use crate::temporal::arg;
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, string_units};

/// A segmenter's.
const LOCALE: usize = 1;
/// A Segments object's and a Segment Iterator's.
const STRING: usize = 1;
/// A Segment Iterator's ([[IteratedStringNextSegmentCodeUnitIndex]]).
const NEXT: usize = 2;

const GRANULARITIES: [&str; 3] = ["grapheme", "word", "sentence"];

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
    if let Some(g) = granularity
        && g != "grapheme"
    {
        return Err(Error(
            format!("Error: Intl.Segmenter: granularity \"{g}\" is not supported (\"grapheme\" alone is)"),
            None,
        ));
    }
    if vm.intl.graphemes.is_none() {
        vm.intl.graphemes = Some(icu::Graphemes::new().map_err(data_error)?);
    }
    let locale = crate::temporal::string(vm, &r.locale);
    Ok(create(vm, proto, Kind::Segmenter, &[locale]))
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
    let granularity = crate::temporal::string(vm, "grapheme");
    let a = vm.intern_name("granularity");
    vm.add_property(r, a, granularity);
    Ok(object_value(r))
}

/// `Intl.Segmenter.prototype.segment(string)`: a Segments object
/// (CreateSegmentsObject).
fn segment(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    require(this, Kind::Segmenter, "Intl.Segmenter.prototype.segment")?;
    let string = crate::intrinsics::to_string(vm, arg(a, 0))?;
    let proto = object_value(root(vm, Root::SegmentsPrototype));
    Ok(create(vm, proto, Kind::Segments, &[string]))
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
    // FindBoundary before and after `n`: the segments from the last
    // sure boundary before it (after a line feed), up to its own.
    let graphemes = vm.intl.graphemes.as_ref().unwrap();
    let mut start = units[..n].iter().rposition(|&u| u == 0x0A).map_or(0, |i| i + 1);
    let mut end = start + graphemes.first(&units[start..]);
    while end <= n {
        start = end;
        end += graphemes.first(&units[end..]);
    }
    segment_data(vm, o, start, end)
}

/// `%IntlSegmentsPrototype%[Symbol.iterator]()`: a Segment Iterator
/// (CreateSegmentIterator).
fn segments_iterator(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = require(this, Kind::Segments, "%IntlSegmentsPrototype%[Symbol.iterator]")?;
    let proto = object_value(root(vm, Root::SegmentIteratorPrototype));
    Ok(create(vm, proto, Kind::SegmentIterator, &[slot(o, STRING), Value::number(0.0)]))
}

/// `%IntlSegmentIteratorPrototype%.next()`.
fn iterator_next(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let o = require(this, Kind::SegmentIterator, "%IntlSegmentIteratorPrototype%.next")?;
    let units = string_units(slot(o, STRING)).unwrap_or(&[]);
    let start = slot(o, NEXT).to_number() as usize;
    if start >= units.len() {
        return crate::symbol::iter_result(vm, Value::UNDEFINED, true);
    }
    // (A boundary found from the last one is the one found from the
    // string's start: no rule of UAX #29 looks back past a boundary.)
    let end = start + vm.intl.graphemes.as_ref().unwrap().first(&units[start..]);
    crate::promise::set_slot(o, NEXT, Value::number(end as f64));
    let data = segment_data(vm, o, start, end)?;
    crate::symbol::iter_result(vm, data, false)
}

/// CreateSegmentDataObject: `{segment, index, input}` of the segment
/// from `start` to `end` of the string of `o` (a Segments object or a
/// Segment Iterator).
fn segment_data(vm: &mut Vm, o: *mut Object, start: usize, end: usize) -> Result<Value> {
    // (Copied: making the string may move the one it is cut from.)
    let piece = string_units(slot(o, STRING)).unwrap_or(&[])[start..end].to_vec();
    let segment = vm.new_string(&piece);
    let op = object_value(vm.object_prototype);
    let r = vm.new_object(op);
    let a = vm.intern_name("segment");
    vm.add_property(r, a, segment);
    let a = vm.intern_name("index");
    vm.add_property(r, a, Value::number(start as f64));
    let a = vm.intern_name("input");
    vm.add_property(r, a, slot(o, STRING));
    Ok(object_value(r))
}
