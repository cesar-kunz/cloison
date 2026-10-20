//! Temporal (ECMA-262's Stage 3 proposal), with the ISO 8601 calendar
//! alone (as an implementation without ECMA-402 has it), and time zones
//! from the system's database (see [`tz`]).
//!
//! A Temporal object is of class `Temporal`: its internal slots, a
//! [`Slots`] value, are bytes its `elems` points to (a cell of bytes,
//! which the collector doesn't look into); a ZonedDateTime's time zone is
//! an index into the VM's interned zones ([`State::zones`]). Exact times
//! are `i128` nanoseconds, duration fields `f64`s (integers, as the spec
//! keeps them).
//!
//! The abstract operations follow the spec's (and its reference
//! implementation's) order of observable steps: property reads, option
//! reads, conversions.

pub(crate) mod iso;
pub(crate) mod parse;
pub(crate) mod tz;
mod duration;
mod fields;
mod plain;
mod zoned;

use std::collections::HashMap;

use crate::builtins::is_class;
use crate::object::{Class, Object, as_object, object_value};
use crate::props::is_object;
use crate::value::Value;
use crate::vm::{Error, NativeFn, Result, Vm, string_units};
use iso::{DateTime, IsoDate, NS_PER_DAY, RoundingMode, Time, Unit};

pub(crate) use duration::Dur;

/// A Temporal object's internal slots.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Slots {
    Instant(i128),
    /// Its exact time, its time zone (see [`State::zones`]).
    Zoned(i128, u32),
    DateTime(DateTime),
    Date(IsoDate),
    Time(Time),
    /// Its year and month, and reference day.
    YearMonth(IsoDate),
    /// Its month and day, and reference year.
    MonthDay(IsoDate),
    Duration(Dur),
}

/// The classes, indices into [`State::protos`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Kind {
    Instant,
    Zoned,
    DateTime,
    Date,
    Time,
    YearMonth,
    MonthDay,
    Duration,
}

impl Slots {
    fn kind(&self) -> Kind {
        match self {
            Slots::Instant(_) => Kind::Instant,
            Slots::Zoned(..) => Kind::Zoned,
            Slots::DateTime(_) => Kind::DateTime,
            Slots::Date(_) => Kind::Date,
            Slots::Time(_) => Kind::Time,
            Slots::YearMonth(_) => Kind::YearMonth,
            Slots::MonthDay(_) => Kind::MonthDay,
            Slots::Duration(_) => Kind::Duration,
        }
    }

    /// Whether it has a [[Calendar]] slot.
    fn has_calendar(&self) -> bool {
        !matches!(self, Slots::Instant(_) | Slots::Time(_) | Slots::Duration(_))
    }
}

/// The VM's Temporal state: the classes' prototypes and constructors, the
/// time zones in use, the system's named zones.
#[derive(Default)]
pub(crate) struct State {
    protos: Vec<*mut Object>,
    ctors: Vec<*mut Object>,
    zones: Vec<tz::Zone>,
    zone_ids: HashMap<String, u32>,
    registry: tz::Registry,
}

impl State {
    pub(crate) fn roots(&self) -> impl Iterator<Item = Value> + '_ {
        self.protos.iter().chain(self.ctors.iter()).map(|&o| object_value(o))
    }
}

// ---- Objects ----

/// A Temporal object's slots, if `v` is one.
pub(crate) fn slots(v: Value) -> Option<Slots> {
    if !is_class(v, Class::Temporal) {
        return None;
    }
    Some(unsafe { (*as_object(v)).elems.cast::<Slots>().read_unaligned() })
}

/// A new object of `slots`'s class (its intrinsic prototype).
pub(crate) fn create(vm: &mut Vm, slots: Slots) -> Value {
    let proto = object_value(vm.temporal.protos[slots.kind() as usize]);
    let o = vm.new_object_with(Class::Temporal, proto, 0);
    let bytes = vm.heap.alloc_bytes(size_of::<Slots>());
    unsafe {
        bytes.cast::<Slots>().write_unaligned(slots);
        (*o).elems = bytes.cast();
    }
    object_value(o)
}

fn this_slots(this: Value, kind: Kind) -> Result<Slots> {
    match slots(this) {
        Some(s) if s.kind() == kind => Ok(s),
        _ => Err(Error::type_error(format!("Temporal.{} method called on an incompatible receiver", NAMES[kind as usize]))),
    }
}

const NAMES: [&str; 8] = ["Instant", "ZonedDateTime", "PlainDateTime", "PlainDate", "PlainTime", "PlainYearMonth", "PlainMonthDay", "Duration"];

pub(crate) fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

pub(crate) fn string(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

fn range(msg: impl Into<String>) -> Error {
    Error::range_error(msg.into())
}

fn type_err(msg: impl Into<String>) -> Error {
    Error::type_error(msg.into())
}

// ---- Conversions ----

/// ToNumber, with what its `valueOf` threw.
pub(crate) fn number(vm: &mut Vm, v: Value) -> Result<f64> {
    let d = crate::interp::to_number(v);
    if vm.conv_failed() { Err(vm.take_conv_error()) } else { Ok(d) }
}

/// ToIntegerWithTruncation: a RangeError for NaN or an infinity.
pub(crate) fn to_integer(vm: &mut Vm, v: Value) -> Result<f64> {
    let d = number(vm, v)?;
    if !d.is_finite() {
        return Err(range("invalid number value"));
    }
    Ok(d.trunc() + 0.0)
}

/// ToPositiveIntegerWithTruncation.
fn to_positive_integer(vm: &mut Vm, v: Value, what: &str) -> Result<f64> {
    let d = to_integer(vm, v)?;
    if d <= 0.0 {
        return Err(range(format!("{what} must be a positive integer")));
    }
    Ok(d)
}

/// ToIntegerIfIntegral.
pub(crate) fn to_integer_if_integral(vm: &mut Vm, v: Value) -> Result<f64> {
    let d = number(vm, v)?;
    if !d.is_finite() {
        return Err(range("infinity is out of range"));
    }
    if d.trunc() != d {
        return Err(range(format!("unsupported fractional value {d}")));
    }
    Ok(d + 0.0)
}

/// The string `v` is (a TypeError for anything else).
pub(crate) fn require_string(v: Value) -> Result<String> {
    match string_units(v) {
        Some(u) => Ok(String::from_utf16_lossy(u)),
        None => Err(type_err("expected a string")),
    }
}

/// ToString, as a Rust string.
pub(crate) fn to_rust_string(vm: &mut Vm, v: Value) -> Result<String> {
    let s = crate::intrinsics::to_string(vm, v)?;
    Ok(String::from_utf16_lossy(string_units(s).unwrap_or(&[])))
}

/// ToPrimitive with hint "string".
fn to_primitive_string(vm: &mut Vm, v: Value) -> Result<Value> {
    let p = crate::interp::to_primitive_string(v);
    if vm.conv_failed() { Err(vm.take_conv_error()) } else { Ok(p) }
}

fn get(vm: &mut Vm, o: Value, name: &str) -> Result<Value> {
    let a = vm.intern(name);
    vm.get(o, a)
}

/// An exact time as a BigInt.
pub(crate) fn bigint_of(vm: &mut Vm, ns: i128) -> Value {
    let b = crate::bigint::Big::parse(&ns.to_string()).unwrap_or_else(crate::bigint::Big::zero);
    crate::bigint::new_bigint(vm, b)
}

// ---- Options ----

/// GetOptionsObject.
pub(crate) fn options_object(vm: &mut Vm, v: Value) -> Result<Value> {
    if v.is_undefined() {
        return Ok(object_value(vm.new_object(Value::NULL)));
    }
    if is_object(v) {
        return Ok(v);
    }
    Err(type_err("options must be an object"))
}

/// GetOption of a string among `allowed`: its index, or `fallback` if
/// undefined (`None`: required, a RangeError).
pub(crate) fn option(vm: &mut Vm, options: Value, name: &str, allowed: &[&str], fallback: Option<usize>) -> Result<usize> {
    let v = get(vm, options, name)?;
    if v.is_undefined() {
        return fallback.ok_or_else(|| range(format!("{name} option is required")));
    }
    let s = to_rust_string(vm, v)?;
    allowed.iter().position(|&a| a == s).ok_or_else(|| range(format!("{name} must be one of {}, not {s}", allowed.join(", "))))
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Overflow {
    Constrain,
    Reject,
}

/// GetTemporalOverflowOption.
pub(crate) fn overflow(vm: &mut Vm, options: Value) -> Result<Overflow> {
    let k = option(vm, options, "overflow", &["constrain", "reject"], Some(0))?;
    Ok(if k == 0 { Overflow::Constrain } else { Overflow::Reject })
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Disambiguation {
    Compatible,
    Earlier,
    Later,
    Reject,
}

/// GetTemporalDisambiguationOption.
pub(crate) fn disambiguation(vm: &mut Vm, options: Value) -> Result<Disambiguation> {
    use Disambiguation::*;
    let k = option(vm, options, "disambiguation", &["compatible", "earlier", "later", "reject"], Some(0))?;
    Ok([Compatible, Earlier, Later, Reject][k])
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum OffsetOption {
    Prefer,
    Use,
    Ignore,
    Reject,
}

/// GetTemporalOffsetOption.
pub(crate) fn offset_option(vm: &mut Vm, options: Value, fallback: OffsetOption) -> Result<OffsetOption> {
    use OffsetOption::*;
    let k = option(vm, options, "offset", &["prefer", "use", "ignore", "reject"], Some(fallback as usize))?;
    Ok([Prefer, Use, Ignore, Reject][k])
}

/// GetRoundingModeOption.
pub(crate) fn rounding_mode(vm: &mut Vm, options: Value, fallback: RoundingMode) -> Result<RoundingMode> {
    let k = option(vm, options, "roundingMode", &RoundingMode::NAMES, Some(fallback as usize))?;
    Ok(RoundingMode::ALL[k])
}

/// GetRoundingIncrementOption.
pub(crate) fn rounding_increment(vm: &mut Vm, options: Value) -> Result<i64> {
    let v = get(vm, options, "roundingIncrement")?;
    if v.is_undefined() {
        return Ok(1);
    }
    let d = to_integer(vm, v)?;
    if !(1.0..=1e9).contains(&d) {
        return Err(range(format!("roundingIncrement must be at least 1 and at most 1e9, not {d}")));
    }
    Ok(d as i64)
}

/// ValidateTemporalRoundingIncrement.
pub(crate) fn validate_increment(increment: i64, dividend: i64, inclusive: bool) -> Result<()> {
    let maximum = if inclusive { dividend } else { dividend - 1 };
    if increment > maximum {
        return Err(range(format!("roundingIncrement must be less than {maximum}, not {increment}")));
    }
    if dividend % increment != 0 {
        return Err(range(format!("roundingIncrement must divide evenly into {dividend}")));
    }
    Ok(())
}

/// A unit-valued option: absent, `auto`, or a unit.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum UnitOption {
    Unset,
    Auto,
    Unit(Unit),
}

/// GetTemporalUnitValuedOption (`required`: a RangeError if absent).
pub(crate) fn unit_option(vm: &mut Vm, options: Value, name: &str, required: bool) -> Result<UnitOption> {
    let v = get(vm, options, name)?;
    if v.is_undefined() {
        if required {
            return Err(range(format!("{name} option is required")));
        }
        return Ok(UnitOption::Unset);
    }
    let s = to_rust_string(vm, v)?;
    if s == "auto" {
        return Ok(UnitOption::Auto);
    }
    for (k, u) in Unit::ALL.iter().enumerate() {
        if Unit::SINGULAR[k] == s || Unit::PLURAL[k] == s {
            return Ok(UnitOption::Unit(*u));
        }
    }
    Err(range(format!("{name} must be a unit, not {s}")))
}

/// The unit groups of ValidateTemporalUnitValue.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Group {
    Date,
    Time,
    DateTime,
}

/// ValidateTemporalUnitValue: `auto` allowed if `auto_ok`.
pub(crate) fn validate_unit(value: UnitOption, group: Group, auto_ok: bool) -> Result<()> {
    match value {
        UnitOption::Unset => Ok(()),
        UnitOption::Auto if auto_ok => Ok(()),
        UnitOption::Unit(u) if group == Group::DateTime || (group == Group::Date) == u.is_date() => Ok(()),
        UnitOption::Auto => Err(range("auto is not allowed here")),
        UnitOption::Unit(u) => Err(range(format!("{} is not allowed here", u.name()))),
    }
}

/// GetTemporalFractionalSecondDigitsOption: `None` for `auto`.
pub(crate) fn fractional_digits(vm: &mut Vm, options: Value) -> Result<Option<u8>> {
    let v = get(vm, options, "fractionalSecondDigits")?;
    if v.is_undefined() {
        return Ok(None);
    }
    if !v.is_number() {
        let s = to_rust_string(vm, v)?;
        if s != "auto" {
            return Err(range(format!("fractionalSecondDigits must be 'auto' or 0 through 9, not {s}")));
        }
        return Ok(None);
    }
    let d = v.to_number().floor();
    if !d.is_finite() || !(0.0..=9.0).contains(&d) {
        return Err(range("fractionalSecondDigits must be 'auto' or 0 through 9"));
    }
    Ok(Some(d as u8))
}

/// ToSecondsStringPrecisionRecord: the precision, and the unit and
/// increment to round to.
pub(crate) fn seconds_precision(smallest: UnitOption, digits: Option<u8>) -> (iso::Precision, Unit, i64) {
    use iso::Precision;
    match smallest {
        UnitOption::Unit(Unit::Minute) => return (Precision::Minute, Unit::Minute, 1),
        UnitOption::Unit(Unit::Second) => return (Precision::Digits(0), Unit::Second, 1),
        UnitOption::Unit(Unit::Millisecond) => return (Precision::Digits(3), Unit::Millisecond, 1),
        UnitOption::Unit(Unit::Microsecond) => return (Precision::Digits(6), Unit::Microsecond, 1),
        UnitOption::Unit(Unit::Nanosecond) => return (Precision::Digits(9), Unit::Nanosecond, 1),
        _ => {}
    }
    match digits {
        None => (Precision::Auto, Unit::Nanosecond, 1),
        Some(0) => (Precision::Digits(0), Unit::Second, 1),
        Some(d @ 1..=3) => (Precision::Digits(d), Unit::Millisecond, 10i64.pow(3 - d as u32)),
        Some(d @ 4..=6) => (Precision::Digits(d), Unit::Microsecond, 10i64.pow(6 - d as u32)),
        Some(d) => (Precision::Digits(d), Unit::Nanosecond, 10i64.pow(9 - d as u32)),
    }
}

/// GetTemporalShowCalendarNameOption: auto, always, never, critical.
pub(crate) fn show_calendar(vm: &mut Vm, options: Value) -> Result<usize> {
    option(vm, options, "calendarName", &["auto", "always", "never", "critical"], Some(0))
}

/// FormatCalendarAnnotation (of the ISO calendar).
pub(crate) fn calendar_annotation(show: usize) -> &'static str {
    match show {
        1 => "[u-ca=iso8601]",
        3 => "[!u-ca=iso8601]",
        _ => "",
    }
}

// ---- Calendars ----

/// CanonicalizeCalendar: the ISO calendar is the only one.
pub(crate) fn canonicalize_calendar(id: &str) -> Result<()> {
    if id.eq_ignore_ascii_case("iso8601") {
        Ok(())
    } else {
        Err(range(format!("invalid calendar identifier {id}")))
    }
}

/// ToTemporalCalendarIdentifier.
pub(crate) fn to_calendar(vm: &mut Vm, v: Value) -> Result<()> {
    if slots(v).is_some_and(|s| s.has_calendar()) {
        return Ok(());
    }
    let id = require_string(v)?;
    if canonicalize_calendar(&id).is_ok() {
        return Ok(());
    }
    let calendar = match parse::parse_date_time(&id) {
        Ok(r) => r.calendar,
        Err(_) => match parse::parse_time(&id) {
            Ok((_, c)) => c,
            Err(_) => match parse::parse_year_month(&id) {
                Ok((.., c)) => c,
                Err(_) => parse::parse_month_day(&id).map_err(range)?.3,
            },
        },
    };
    let _ = vm;
    canonicalize_calendar(calendar.as_deref().unwrap_or("iso8601"))
}

/// GetTemporalCalendarIdentifierWithISODefault.
pub(crate) fn calendar_with_iso_default(vm: &mut Vm, item: Value) -> Result<()> {
    if slots(item).is_some_and(|s| s.has_calendar()) {
        return Ok(());
    }
    let c = get(vm, item, "calendar")?;
    if c.is_undefined() {
        return Ok(());
    }
    to_calendar(vm, c)
}

// ---- Time zones ----

/// The interned time zone `zone` (by its identifier).
fn intern_zone(vm: &mut Vm, zone: tz::Zone) -> u32 {
    if let Some(&k) = vm.temporal.zone_ids.get(&zone.id) {
        return k;
    }
    let k = vm.temporal.zones.len() as u32;
    vm.temporal.zone_ids.insert(zone.id.clone(), k);
    vm.temporal.zones.push(zone);
    k
}

pub(crate) fn zone(vm: &Vm, k: u32) -> &tz::Zone {
    &vm.temporal.zones[k as usize]
}

/// The zone a parsed time zone string names (a RangeError if none).
fn zone_of(vm: &mut Vm, name: parse::TzName) -> Result<u32> {
    let z = match name {
        parse::TzName::Offset(m) => tz::Zone { id: iso::offset_minutes_string(m), kind: tz::ZoneKind::Offset(m) },
        parse::TzName::Name(n) => match vm.temporal.registry.find(&n) {
            Some(z) => z,
            None => return Err(range(format!("unrecognized time zone {n}"))),
        },
    };
    Ok(intern_zone(vm, z))
}

/// ToTemporalTimeZoneIdentifier.
pub(crate) fn to_time_zone(vm: &mut Vm, v: Value) -> Result<u32> {
    if let Some(Slots::Zoned(_, z)) = slots(v) {
        return Ok(z);
    }
    let s = require_string(v)?;
    let name = parse::parse_tz_string(&s).map_err(range)?;
    zone_of(vm, name)
}

/// A time zone identifier (an annotation's): ParseTimeZoneIdentifier.
pub(crate) fn time_zone_from_identifier(vm: &mut Vm, id: &str) -> Result<u32> {
    let name = parse::parse_tz_identifier(id).map_err(range)?;
    zone_of(vm, name)
}

/// The system's time zone (Temporal.Now's).
pub(crate) fn system_time_zone(vm: &mut Vm) -> u32 {
    let name = tz::system_zone_name();
    time_zone_from_identifier(vm, &name).unwrap_or_else(|_| {
        let utc = vm.temporal.registry.find("UTC");
        let z = utc.unwrap_or(tz::Zone { id: "UTC".into(), kind: tz::ZoneKind::Offset(0) });
        intern_zone(vm, z)
    })
}

/// A RangeError unless `ns` is a valid exact time.
pub(crate) fn check_epoch_ns(ns: i128) -> Result<i128> {
    if iso::valid_epoch_ns(ns) { Ok(ns) } else { Err(range("date/time value is outside of supported range")) }
}

/// GetPossibleEpochNanoseconds: the exact times of wall time `dt` in
/// zone `z`.
pub(crate) fn possible_epoch_ns(vm: &Vm, z: u32, dt: DateTime) -> Result<Vec<i128>> {
    let zone = zone(vm, z);
    // (An offset zone's UTC date in range, as CheckISODaysRange has it; a
    // named zone's exact times checked below.)
    let utc_date = match zone.kind {
        tz::ZoneKind::Offset(m) => iso::balance_date_time(dt.date, dt.time.nanos() as i128 - m as i128 * 60_000_000_000).date,
        tz::ZoneKind::Named(..) => dt.date,
    };
    if !iso::days_in_range(utc_date) && (matches!(zone.kind, tz::ZoneKind::Offset(_)) || utc_date.year.abs() > 300_000) {
        return Err(range("date/time value is outside of supported range"));
    }
    let out = zone.possible(dt);
    for &ns in &out {
        check_epoch_ns(ns)?;
    }
    Ok(out)
}

/// GetEpochNanosecondsFor.
pub(crate) fn epoch_ns_for(vm: &Vm, z: u32, dt: DateTime, d: Disambiguation) -> Result<i128> {
    let possible = possible_epoch_ns(vm, z, dt)?;
    disambiguate(vm, &possible, z, dt, d)
}

/// DisambiguatePossibleEpochNanoseconds.
pub(crate) fn disambiguate(vm: &Vm, possible: &[i128], z: u32, dt: DateTime, d: Disambiguation) -> Result<i128> {
    match possible.len() {
        1 => return Ok(possible[0]),
        n if n > 1 => {
            return match d {
                Disambiguation::Compatible | Disambiguation::Earlier => Ok(possible[0]),
                Disambiguation::Later => Ok(possible[n - 1]),
                Disambiguation::Reject => Err(range("multiple instants found")),
            };
        }
        _ => {}
    }
    if d == Disambiguation::Reject {
        return Err(range("the wall-clock time is skipped in this time zone"));
    }
    let utc = iso::utc_epoch_ns(dt);
    let before = check_epoch_ns(utc - NS_PER_DAY)?;
    let after = check_epoch_ns(utc + NS_PER_DAY)?;
    let zn = zone(vm, z);
    let shift = (zn.offset_ns(after) - zn.offset_ns(before)) as i128;
    let t = dt.time.nanos() as i128;
    if d == Disambiguation::Earlier {
        let earlier = iso::balance_date_time(dt.date, t - shift);
        return Ok(possible_epoch_ns(vm, z, earlier)?[0]);
    }
    let later = iso::balance_date_time(dt.date, t + shift);
    let p = possible_epoch_ns(vm, z, later)?;
    Ok(p[p.len() - 1])
}

/// GetStartOfDay.
pub(crate) fn start_of_day(vm: &Vm, z: u32, date: IsoDate) -> Result<i128> {
    let dt = DateTime { date, time: Time::MIDNIGHT };
    let possible = possible_epoch_ns(vm, z, dt)?;
    if let Some(&first) = possible.first() {
        return Ok(first);
    }
    let before = check_epoch_ns(iso::utc_epoch_ns(dt) - NS_PER_DAY)?;
    zone(vm, z).next_transition(before).ok_or_else(|| range("no start of day"))
}

/// How InterpretISODateTimeOffset treats the offset.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum OffsetBehaviour {
    Option,
    Exact,
    Wall,
}

/// InterpretISODateTimeOffset: the exact time of a wall time (`None`:
/// the start of the day) in zone `z`, given an offset.
#[allow(clippy::too_many_arguments)]
pub(crate) fn interpret_offset(
    vm: &Vm,
    date: IsoDate,
    time: Option<Time>,
    behaviour: OffsetBehaviour,
    offset_ns: i64,
    z: u32,
    d: Disambiguation,
    offset_opt: OffsetOption,
    match_minutes: bool,
) -> Result<i128> {
    let Some(time) = time else { return start_of_day(vm, z, date) };
    let dt = DateTime { date, time };
    if behaviour == OffsetBehaviour::Wall || offset_opt == OffsetOption::Ignore {
        return epoch_ns_for(vm, z, dt, d);
    }
    if behaviour == OffsetBehaviour::Exact || offset_opt == OffsetOption::Use {
        let balanced = iso::balance_date_time(date, time.nanos() as i128 - offset_ns as i128);
        if !iso::days_in_range(balanced.date) {
            return Err(range("date/time value is outside of supported range"));
        }
        return check_epoch_ns(iso::utc_epoch_ns(balanced));
    }
    if !iso::days_in_range(date) {
        return Err(range("date/time value is outside of supported range"));
    }
    let utc = iso::utc_epoch_ns(dt);
    let possible = possible_epoch_ns(vm, z, dt)?;
    for &candidate in &possible {
        let off = utc - candidate;
        let rounded = iso::round_to_increment(off, 60_000_000_000, RoundingMode::HalfExpand);
        if off == offset_ns as i128 || (match_minutes && rounded == offset_ns as i128) {
            return Ok(candidate);
        }
    }
    if offset_opt == OffsetOption::Reject {
        return Err(range(format!(
            "offset {} is invalid for {} in {}",
            iso::offset_string(offset_ns),
            iso::date_time_string(dt, iso::Precision::Auto),
            zone(vm, z).id
        )));
    }
    disambiguate(vm, &possible, z, dt, d)
}

// ---- Ranges ----

/// RejectDateTimeRange.
pub(crate) fn check_date_time(dt: DateTime) -> Result<DateTime> {
    if iso::date_time_within_limits(dt) { Ok(dt) } else { Err(range("date/time is outside of supported range")) }
}

/// RejectDateRange (a valid date, and within the limits at noon).
pub(crate) fn check_date(d: IsoDate) -> Result<IsoDate> {
    if iso::date_within_limits(d) { Ok(d) } else { Err(range(format!("{} is outside of supported range", iso::date_string(d)))) }
}

/// RejectYearMonthRange.
pub(crate) fn check_year_month(d: IsoDate) -> Result<IsoDate> {
    if iso::year_month_within_limits(d.year, d.month as i64) { Ok(d) } else { Err(range("year-month is outside of supported range")) }
}

/// RejectISODate of numbers (any): an `IsoDate`.
pub(crate) fn reject_iso_date(y: f64, m: f64, d: f64) -> Result<IsoDate> {
    if !(1.0..=12.0).contains(&m) || y.abs() > 1e15 {
        return Err(range("date is out of range"));
    }
    if d < 1.0 || d > iso::days_in_month(y as i64, m as i64) as f64 {
        return Err(range("date is out of range"));
    }
    Ok(IsoDate::new(y as i64, m as i64, d as i64))
}

/// RegulateISODate.
pub(crate) fn regulate_iso_date(y: f64, m: f64, d: f64, overflow: Overflow) -> Result<IsoDate> {
    match overflow {
        Overflow::Reject => reject_iso_date(y, m, d),
        Overflow::Constrain => {
            if y.abs() > 1e15 {
                return Err(range("date is out of range"));
            }
            let m = m.clamp(1.0, 12.0);
            let d = d.clamp(1.0, iso::days_in_month(y as i64, m as i64) as f64);
            Ok(IsoDate::new(y as i64, m as i64, d as i64))
        }
    }
}

/// RejectTime.
pub(crate) fn reject_time(f: [f64; 6]) -> Result<Time> {
    let max = [23.0, 59.0, 59.0, 999.0, 999.0, 999.0];
    if f.iter().zip(max).any(|(&v, m)| !(0.0..=m).contains(&v)) {
        return Err(range("time is out of range"));
    }
    Ok(time_of(f))
}

fn time_of(f: [f64; 6]) -> Time {
    Time {
        hour: f[0] as u8,
        minute: f[1] as u8,
        second: f[2] as u8,
        millisecond: f[3] as u16,
        microsecond: f[4] as u16,
        nanosecond: f[5] as u16,
    }
}

/// RegulateTime.
pub(crate) fn regulate_time(f: [f64; 6], overflow: Overflow) -> Result<Time> {
    match overflow {
        Overflow::Reject => reject_time(f),
        Overflow::Constrain => {
            let max = [23.0, 59.0, 59.0, 999.0, 999.0, 999.0];
            let mut c = f;
            for (v, m) in c.iter_mut().zip(max) {
                *v = v.clamp(0.0, m);
            }
            Ok(time_of(c))
        }
    }
}

// ---- Strings ----

pub(crate) fn parse_err(e: String) -> Error {
    range(e)
}

/// `valueOf`: Temporal objects can't be compared or added as primitives.
fn value_of(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Err(type_err("use Temporal compare() or equals(), not arithmetic or comparison operators"))
}

// ---- Install ----

/// A class: its constructor (`length`), its statics, getters and methods.
pub(crate) struct ClassSpec {
    pub name: &'static str,
    pub kind: Kind,
    pub ctor: NativeFn,
    pub length: u32,
    pub statics: &'static [(&'static str, NativeFn, u32)],
    pub getters: &'static [(&'static str, NativeFn)],
    pub methods: &'static [(&'static str, NativeFn, u32)],
}

pub(crate) fn install(vm: &mut Vm) {
    let g = vm.global;
    let op = object_value(vm.object_prototype);
    let any = crate::jit::Ty::Any;
    let temporal = vm.new_object(op);
    let a = vm.intern("Temporal");
    vm.add_property_with(g, a, object_value(temporal), crate::object::DONT_ENUM);
    crate::symbol::to_string_tag(vm, temporal, "Temporal");
    vm.temporal.protos = vec![std::ptr::null_mut(); 8];
    vm.temporal.ctors = vec![std::ptr::null_mut(); 8];
    let specs = [
        zoned::instant_spec(),
        zoned::zoned_spec(),
        plain::date_time_spec(),
        plain::date_spec(),
        plain::time_spec(),
        plain::year_month_spec(),
        plain::month_day_spec(),
        duration::spec(),
    ];
    for spec in specs {
        let proto = vm.new_object(op);
        let c = vm.define_native(temporal, spec.name, spec.ctor, spec.length, any, true);
        crate::builtins::link_constructor(vm, c, proto);
        vm.define_natives(c, spec.statics);
        for &(name, f) in spec.getters {
            crate::builtins::getter(vm, proto, name, f);
        }
        vm.define_natives(proto, spec.methods);
        vm.define_natives(proto, &[("valueOf", value_of as NativeFn, 0)]);
        crate::symbol::to_string_tag(vm, proto, &format!("Temporal.{}", spec.name));
        vm.temporal.protos[spec.kind as usize] = proto;
        vm.temporal.ctors[spec.kind as usize] = c;
    }
    zoned::install_now(vm, temporal);
}
