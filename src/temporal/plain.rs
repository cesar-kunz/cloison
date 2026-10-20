//! Temporal.PlainDate, PlainTime, PlainDateTime, PlainYearMonth and
//! PlainMonthDay, and the date and time getters the classes share.

use super::duration::{self, ZERO_DATE};
use super::fields::{self, Field, Required};
use super::iso::{self, DateTime, IsoDate, RoundingMode, Time, Unit};
use super::*;

// ---- Shared getters ----

/// The ISO date of a Temporal object of kind `K` (a ZonedDateTime's in
/// its time zone).
fn date_of(vm: &Vm, this: Value, k: usize) -> Result<IsoDate> {
    let kind = [Kind::Instant, Kind::Zoned, Kind::DateTime, Kind::Date, Kind::Time, Kind::YearMonth, Kind::MonthDay, Kind::Duration][k];
    Ok(match this_slots(this, kind)? {
        Slots::Date(d) | Slots::YearMonth(d) | Slots::MonthDay(d) => d,
        Slots::DateTime(dt) => dt.date,
        Slots::Zoned(ns, z) => zone(vm, z).date_time_at(ns).date,
        _ => unreachable!(),
    })
}

fn time_of_this(vm: &Vm, this: Value, k: usize) -> Result<Time> {
    let kind = [Kind::Instant, Kind::Zoned, Kind::DateTime, Kind::Date, Kind::Time][k];
    Ok(match this_slots(this, kind)? {
        Slots::Time(t) => t,
        Slots::DateTime(dt) => dt.time,
        Slots::Zoned(ns, z) => zone(vm, z).date_time_at(ns).time,
        _ => unreachable!(),
    })
}

/// A date getter (`F` indexes [`DATE_GETTERS`]) of class `K`.
pub(crate) fn date_getter<const K: usize, const F: usize>(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = date_of(vm, this, K)?;
    Ok(match F {
        0 => Value::number(d.year as f64),
        1 => Value::from_i32(d.month as i32),
        2 => string(vm, &format!("M{:02}", d.month)),
        3 => Value::from_i32(d.day as i32),
        4 => Value::from_i32(iso::day_of_week(d) as i32),
        5 => Value::from_i32(iso::day_of_year(d) as i32),
        6 => Value::from_i32(iso::week_of_year(d).0 as i32),
        7 => Value::number(iso::week_of_year(d).1 as f64),
        8 => Value::from_i32(7),
        9 => Value::from_i32(iso::days_in_month(d.year, d.month as i64) as i32),
        10 => Value::from_i32(iso::days_in_year(d.year) as i32),
        11 => Value::from_i32(12),
        12 => Value::from_bool(iso::is_leap(d.year)),
        13 | 14 => Value::UNDEFINED,
        _ => string(vm, "iso8601"),
    })
}

/// A time getter (hour, minute, second, millisecond, microsecond,
/// nanosecond) of class `K`.
pub(crate) fn time_getter<const K: usize, const F: usize>(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let t = time_of_this(vm, this, K)?;
    Ok(Value::from_i32(t.fields()[F] as i32))
}

/// RejectTemporalLikeObject: a `with` argument can't be a Temporal object
/// with a calendar or time zone, a PlainTime, or have a `calendar` or
/// `timeZone` property.
pub(crate) fn reject_temporal_like(vm: &mut Vm, item: Value) -> Result<()> {
    if !is_object(item) {
        return Err(type_err("invalid argument: not an object"));
    }
    if let Some(s) = slots(item) {
        if s.has_calendar() || matches!(s, Slots::Time(_)) {
            return Err(type_err("with() does not accept a Temporal object"));
        }
    }
    for name in ["calendar", "timeZone"] {
        if !get(vm, item, name)?.is_undefined() {
            return Err(type_err(format!("with() does not support a {name} property")));
        }
    }
    Ok(())
}

/// `round(roundTo)`'s options: a string the smallest unit.
fn round_options(vm: &mut Vm, v: Value) -> Result<Value> {
    if v.is_undefined() {
        return Err(type_err("options parameter is required"));
    }
    if string_units(v).is_some() {
        let o = object_value(vm.new_object(Value::NULL));
        let a = vm.intern("smallestUnit");
        vm.add_property(as_object(o), a, v);
        return Ok(o);
    }
    options_object(vm, v)
}

/// RoundTime: the days carried over, and the time.
pub(crate) fn round_time(t: Time, increment: i64, unit: Unit, mode: RoundingMode) -> (i64, Time) {
    let f = t.fields().map(|v| v as i128);
    let quantity = match unit {
        Unit::Day | Unit::Hour => iso::time_nanos(f[0], f[1], f[2], f[3], f[4], f[5]),
        Unit::Minute => iso::time_nanos(0, f[1], f[2], f[3], f[4], f[5]),
        Unit::Second => iso::time_nanos(0, 0, f[2], f[3], f[4], f[5]),
        Unit::Millisecond => iso::time_nanos(0, 0, 0, f[3], f[4], f[5]),
        Unit::Microsecond => f[4] * 1000 + f[5],
        _ => f[5],
    };
    let unit_ns = unit.nanos();
    let r = iso::round_to_increment(quantity, unit_ns * increment as i128, mode) / unit_ns;
    if unit == Unit::Day {
        return (r as i64, Time::MIDNIGHT);
    }
    let ns = match unit {
        Unit::Hour => iso::time_nanos(r, 0, 0, 0, 0, 0),
        Unit::Minute => iso::time_nanos(f[0], r, 0, 0, 0, 0),
        Unit::Second => iso::time_nanos(f[0], f[1], r, 0, 0, 0),
        Unit::Millisecond => iso::time_nanos(f[0], f[1], f[2], r, 0, 0),
        Unit::Microsecond => iso::time_nanos(f[0], f[1], f[2], f[3], r, 0),
        _ => iso::time_nanos(f[0], f[1], f[2], f[3], f[4], r),
    };
    iso::balance_time(ns)
}

/// RoundISODateTime.
pub(crate) fn round_date_time(dt: DateTime, increment: i64, unit: Unit, mode: RoundingMode) -> DateTime {
    let (days, time) = round_time(dt.time, increment, unit, mode);
    DateTime { date: iso::add_days(dt.date, days), time }
}

fn since_until(vm: &mut Vm, d: duration::Dur, since: bool) -> Value {
    let d = if since { duration::negated(&d) } else { d };
    duration::create_duration(vm, d)
}

// ---- Temporal.PlainDate ----

fn this_date(this: Value) -> Result<IsoDate> {
    match this_slots(this, Kind::Date)? {
        Slots::Date(d) => Ok(d),
        _ => unreachable!(),
    }
}

pub(crate) fn create_date(vm: &mut Vm, d: IsoDate) -> Result<Value> {
    Ok(create(vm, Slots::Date(check_date(d)?)))
}

/// A constructor's calendar argument: a string, the ISO calendar's.
fn calendar_arg(v: Value) -> Result<()> {
    if v.is_undefined() {
        return Ok(());
    }
    canonicalize_calendar(&require_string(v)?)
}

fn date_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.PlainDate requires 'new'"));
    }
    let y = to_integer(vm, arg(a, 0))?;
    let m = to_integer(vm, arg(a, 1))?;
    let d = to_integer(vm, arg(a, 2))?;
    calendar_arg(arg(a, 3))?;
    let date = reject_iso_date(y, m, d)?;
    create_date(vm, date)
}

fn date_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let item = arg(a, 0);
    reject_temporal_like(vm, item)?;
    let base = fields::date_fields(d, true, true);
    let partial = fields::prepare(vm, item, fields::DATE_FIELDS, Required::Partial)?;
    let merged = fields::merge(&base, &partial);
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let r = fields::date_from_fields(&merged, ov)?;
    create_date(vm, r)
}

fn date_with_calendar(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    to_calendar(vm, arg(a, 0))?;
    create_date(vm, d)
}

/// AddDurationToDate.
fn date_add_impl(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let d = this_date(this)?;
    let mut dur = duration::to_duration(vm, arg(a, 0))?;
    if subtract {
        dur = duration::negated(&dur);
    }
    let dd = duration::date_without_time(&dur)?;
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let r = fields::date_add(d, &dd, ov)?;
    create_date(vm, r)
}

fn date_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_add_impl(vm, this, a, false)
}

fn date_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_add_impl(vm, this, a, true)
}

/// DifferenceTemporalPlainDate.
fn date_difference(vm: &mut Vm, this: Value, a: &[Value], since: bool) -> Result<Value> {
    let d = this_date(this)?;
    let other = fields::to_date(vm, arg(a, 0), Value::UNDEFINED)?;
    let o = options_object(vm, arg(a, 1))?;
    let s = duration::difference_settings(vm, since, o, Group::Date, &[], Unit::Day, Unit::Day)?;
    if d == other {
        return Ok(duration::create_duration(vm, [0.0; 10]));
    }
    let diff = fields::date_until(d, other, s.largest);
    let mut internal = duration::Internal { date: diff, time: 0 };
    if !(s.smallest == Unit::Day && s.increment == 1) {
        let start = DateTime { date: d, time: Time::MIDNIGHT };
        let end = DateTime { date: other, time: Time::MIDNIGHT };
        internal = duration::difference_plain_rounded(vm, start, end, s.largest, s.increment, s.smallest, s.mode)?;
    }
    let r = duration::from_internal(&internal, Unit::Day)?;
    Ok(since_until(vm, r, since))
}

fn date_until(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_difference(vm, this, a, false)
}

fn date_since(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_difference(vm, this, a, true)
}

fn date_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let other = fields::to_date(vm, arg(a, 0), Value::UNDEFINED)?;
    Ok(Value::from_bool(d == other))
}

fn date_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let show = show_calendar(vm, o)?;
    Ok(string(vm, &format!("{}{}", iso::date_string(d), calendar_annotation(show))))
}

fn date_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    Ok(string(vm, &iso::date_string(d)))
}

fn date_to_plain_date_time(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let t = fields::to_time_or_midnight(vm, arg(a, 0))?;
    create_date_time(vm, DateTime { date: d, time: t })
}

fn date_to_zoned(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let item = arg(a, 0);
    let (z, time) = if is_object(item) {
        let tz = get(vm, item, "timeZone")?;
        if tz.is_undefined() {
            (to_time_zone(vm, item)?, Value::UNDEFINED)
        } else {
            let z = to_time_zone(vm, tz)?;
            (z, get(vm, item, "plainTime")?)
        }
    } else {
        (to_time_zone(vm, item)?, Value::UNDEFINED)
    };
    let ns = if time.is_undefined() {
        start_of_day(vm, z, d)?
    } else {
        let t = fields::to_time(vm, time, Value::UNDEFINED)?;
        epoch_ns_for(vm, z, DateTime { date: d, time: t }, Disambiguation::Compatible)?
    };
    super::zoned::create_zoned(vm, ns, z)
}

fn date_to_plain_year_month(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let f = fields::date_fields(d, true, true);
    let r = fields::year_month_from_fields(&f, Overflow::Constrain)?;
    create_year_month(vm, r)
}

fn date_to_plain_month_day(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_date(this)?;
    let f = fields::date_fields(d, true, true);
    let r = fields::month_day_from_fields(&f, Overflow::Constrain)?;
    create_month_day(vm, r)
}

fn date_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let d = fields::to_date(vm, arg(a, 0), arg(a, 1))?;
    create_date(vm, d)
}

fn date_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let one = fields::to_date(vm, arg(a, 0), Value::UNDEFINED)?;
    let two = fields::to_date(vm, arg(a, 1), Value::UNDEFINED)?;
    Ok(Value::from_i32(one.cmp(&two) as i32))
}

const DATE: usize = Kind::Date as usize;

pub(crate) fn date_spec() -> ClassSpec {
    ClassSpec {
        name: "PlainDate",
        kind: Kind::Date,
        ctor: date_ctor,
        length: 3,
        statics: &[("from", date_from as NativeFn, 1), ("compare", date_compare, 2)],
        getters: &[
            ("calendarId", date_getter::<DATE, 15>),
            ("era", date_getter::<DATE, 13>),
            ("eraYear", date_getter::<DATE, 14>),
            ("year", date_getter::<DATE, 0>),
            ("month", date_getter::<DATE, 1>),
            ("monthCode", date_getter::<DATE, 2>),
            ("day", date_getter::<DATE, 3>),
            ("dayOfWeek", date_getter::<DATE, 4>),
            ("dayOfYear", date_getter::<DATE, 5>),
            ("weekOfYear", date_getter::<DATE, 6>),
            ("yearOfWeek", date_getter::<DATE, 7>),
            ("daysInWeek", date_getter::<DATE, 8>),
            ("daysInMonth", date_getter::<DATE, 9>),
            ("daysInYear", date_getter::<DATE, 10>),
            ("monthsInYear", date_getter::<DATE, 11>),
            ("inLeapYear", date_getter::<DATE, 12>),
        ],
        methods: &[
            ("with", date_with as NativeFn, 1),
            ("withCalendar", date_with_calendar, 1),
            ("add", date_add, 1),
            ("subtract", date_subtract, 1),
            ("until", date_until, 1),
            ("since", date_since, 1),
            ("equals", date_equals, 1),
            ("toString", date_to_string, 0),
            ("toJSON", date_to_json, 0),
            ("toLocaleString", date_to_json, 0),
            ("toPlainDateTime", date_to_plain_date_time, 0),
            ("toZonedDateTime", date_to_zoned, 1),
            ("toPlainYearMonth", date_to_plain_year_month, 0),
            ("toPlainMonthDay", date_to_plain_month_day, 0),
        ],
    }
}

// ---- Temporal.PlainTime ----

fn this_time(this: Value) -> Result<Time> {
    match this_slots(this, Kind::Time)? {
        Slots::Time(t) => Ok(t),
        _ => unreachable!(),
    }
}

pub(crate) fn create_time(vm: &mut Vm, t: Time) -> Value {
    create(vm, Slots::Time(t))
}

fn time_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.PlainTime requires 'new'"));
    }
    let mut f = [0f64; 6];
    for (k, v) in f.iter_mut().enumerate() {
        let x = arg(a, k);
        *v = if x.is_undefined() { 0.0 } else { to_integer(vm, x)? };
    }
    let t = reject_time(f)?;
    Ok(create_time(vm, t))
}

fn time_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    let item = arg(a, 0);
    reject_temporal_like(vm, item)?;
    let partial = fields::time_record(vm, item, true)?;
    let base = t.fields();
    let merged: [f64; 6] = std::array::from_fn(|k| partial[k].unwrap_or(base[k] as f64));
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let r = regulate_time(merged, ov)?;
    Ok(create_time(vm, r))
}

/// AddDurationToTime.
fn time_add_impl(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let t = this_time(this)?;
    let mut d = duration::to_duration(vm, arg(a, 0))?;
    if subtract {
        d = duration::negated(&d);
    }
    let i = duration::to_internal_24h(&d);
    let (_, r) = iso::balance_time(t.nanos() as i128 + i.time);
    Ok(create_time(vm, r))
}

fn time_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    time_add_impl(vm, this, a, false)
}

fn time_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    time_add_impl(vm, this, a, true)
}

/// DifferenceTemporalPlainTime.
fn time_difference(vm: &mut Vm, this: Value, a: &[Value], since: bool) -> Result<Value> {
    let t = this_time(this)?;
    let other = fields::to_time(vm, arg(a, 0), Value::UNDEFINED)?;
    let o = options_object(vm, arg(a, 1))?;
    let s = duration::difference_settings(vm, since, o, Group::Time, &[], Unit::Nanosecond, Unit::Hour)?;
    let diff = duration::difference_time(t, other);
    let diff = duration::round_time_duration(diff, s.increment, s.smallest, s.mode)?;
    let r = duration::from_internal(&duration::Internal { date: ZERO_DATE, time: diff }, s.largest)?;
    Ok(since_until(vm, r, since))
}

fn time_until(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    time_difference(vm, this, a, false)
}

fn time_since(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    time_difference(vm, this, a, true)
}

fn time_round(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    let o = round_options(vm, arg(a, 0))?;
    let increment = rounding_increment(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::HalfExpand)?;
    let smallest = unit_option(vm, o, "smallestUnit", true)?;
    validate_unit(smallest, Group::Time, false)?;
    let UnitOption::Unit(unit) = smallest else { return Err(range("smallestUnit is required")) };
    validate_increment(increment, duration::max_increment(unit).unwrap(), false)?;
    let (_, r) = round_time(t, increment, unit, mode);
    Ok(create_time(vm, r))
}

fn time_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    let other = fields::to_time(vm, arg(a, 0), Value::UNDEFINED)?;
    Ok(Value::from_bool(t == other))
}

fn time_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let digits = fractional_digits(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::Trunc)?;
    let smallest = unit_option(vm, o, "smallestUnit", false)?;
    validate_unit(smallest, Group::Time, false)?;
    if smallest == UnitOption::Unit(Unit::Hour) {
        return Err(range("smallestUnit must be a time unit other than hour"));
    }
    let (precision, unit, increment) = seconds_precision(smallest, digits);
    let (_, r) = round_time(t, increment, unit, mode);
    Ok(string(vm, &iso::time_record_string(r, precision)))
}

fn time_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    Ok(string(vm, &iso::time_record_string(t, iso::Precision::Auto)))
}

fn time_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let t = fields::to_time(vm, arg(a, 0), arg(a, 1))?;
    Ok(create_time(vm, t))
}

fn time_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let one = fields::to_time(vm, arg(a, 0), Value::UNDEFINED)?;
    let two = fields::to_time(vm, arg(a, 1), Value::UNDEFINED)?;
    Ok(Value::from_i32(one.cmp(&two) as i32))
}

const TIME: usize = Kind::Time as usize;

pub(crate) fn time_spec() -> ClassSpec {
    ClassSpec {
        name: "PlainTime",
        kind: Kind::Time,
        ctor: time_ctor,
        length: 0,
        statics: &[("from", time_from as NativeFn, 1), ("compare", time_compare, 2)],
        getters: &[
            ("hour", time_getter::<TIME, 0>),
            ("minute", time_getter::<TIME, 1>),
            ("second", time_getter::<TIME, 2>),
            ("millisecond", time_getter::<TIME, 3>),
            ("microsecond", time_getter::<TIME, 4>),
            ("nanosecond", time_getter::<TIME, 5>),
        ],
        methods: &[
            ("with", time_with as NativeFn, 1),
            ("add", time_add, 1),
            ("subtract", time_subtract, 1),
            ("until", time_until, 1),
            ("since", time_since, 1),
            ("round", time_round, 1),
            ("equals", time_equals, 1),
            ("toString", time_to_string, 0),
            ("toJSON", time_to_json, 0),
            ("toLocaleString", time_to_json, 0),
        ],
    }
}

// ---- Temporal.PlainDateTime ----

fn this_date_time(this: Value) -> Result<DateTime> {
    match this_slots(this, Kind::DateTime)? {
        Slots::DateTime(dt) => Ok(dt),
        _ => unreachable!(),
    }
}

pub(crate) fn create_date_time(vm: &mut Vm, dt: DateTime) -> Result<Value> {
    Ok(create(vm, Slots::DateTime(check_date_time(dt)?)))
}

fn date_time_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.PlainDateTime requires 'new'"));
    }
    let y = to_integer(vm, arg(a, 0))?;
    let m = to_integer(vm, arg(a, 1))?;
    let d = to_integer(vm, arg(a, 2))?;
    let mut f = [0f64; 6];
    for (k, v) in f.iter_mut().enumerate() {
        let x = arg(a, 3 + k);
        *v = if x.is_undefined() { 0.0 } else { to_integer(vm, x)? };
    }
    calendar_arg(arg(a, 9))?;
    let date = reject_iso_date(y, m, d)?;
    let time = reject_time(f)?;
    create_date_time(vm, DateTime { date, time })
}

fn date_time_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    let item = arg(a, 0);
    reject_temporal_like(vm, item)?;
    let mut base = fields::date_fields(dt.date, true, true);
    base.time = dt.time.fields().map(|v| Some(v as f64));
    let which: Vec<Field> = fields::DATE_FIELDS.iter().chain(fields::TIME_FIELDS).copied().collect();
    let partial = fields::prepare(vm, item, &which, Required::Partial)?;
    let merged = fields::merge(&base, &partial);
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let date = fields::date_from_fields(&merged, ov)?;
    let time = fields::time_from_fields(&merged, ov)?;
    create_date_time(vm, DateTime { date, time })
}

fn date_time_with_plain_time(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    let t = fields::to_time_or_midnight(vm, arg(a, 0))?;
    create_date_time(vm, DateTime { date: dt.date, time: t })
}

fn date_time_with_calendar(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    to_calendar(vm, arg(a, 0))?;
    create_date_time(vm, dt)
}

/// AddDurationToDateTime.
fn date_time_add_impl(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let dt = this_date_time(this)?;
    let mut d = duration::to_duration(vm, arg(a, 0))?;
    if subtract {
        d = duration::negated(&d);
    }
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let i = duration::to_internal_24h(&d);
    let (days, time) = iso::balance_time(dt.time.nanos() as i128 + i.time);
    let dd = duration::adjust(&i.date, days as f64, None, None)?;
    let date = fields::date_add(dt.date, &dd, ov)?;
    create_date_time(vm, DateTime { date, time })
}

fn date_time_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_time_add_impl(vm, this, a, false)
}

fn date_time_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_time_add_impl(vm, this, a, true)
}

/// DifferenceTemporalPlainDateTime.
fn date_time_difference(vm: &mut Vm, this: Value, a: &[Value], since: bool) -> Result<Value> {
    let dt = this_date_time(this)?;
    let other = fields::to_date_time(vm, arg(a, 0), Value::UNDEFINED)?;
    let o = options_object(vm, arg(a, 1))?;
    let s = duration::difference_settings(vm, since, o, Group::DateTime, &[], Unit::Nanosecond, Unit::Day)?;
    if dt == other {
        return Ok(duration::create_duration(vm, [0.0; 10]));
    }
    let i = duration::difference_plain_rounded(vm, dt, other, s.largest, s.increment, s.smallest, s.mode)?;
    let r = duration::from_internal(&i, s.largest)?;
    Ok(since_until(vm, r, since))
}

fn date_time_until(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_time_difference(vm, this, a, false)
}

fn date_time_since(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    date_time_difference(vm, this, a, true)
}

fn date_time_round(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    let o = round_options(vm, arg(a, 0))?;
    let increment = rounding_increment(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::HalfExpand)?;
    let smallest = unit_option(vm, o, "smallestUnit", true)?;
    if smallest != UnitOption::Unit(Unit::Day) {
        validate_unit(smallest, Group::Time, false)?;
    }
    let UnitOption::Unit(unit) = smallest else { return Err(range("smallestUnit is required")) };
    let max = duration::max_increment(unit).unwrap_or(1);
    validate_increment(increment, max, max == 1)?;
    if increment == 1 && unit == Unit::Nanosecond {
        return create_date_time(vm, dt);
    }
    let r = round_date_time(dt, increment, unit, mode);
    create_date_time(vm, r)
}

fn date_time_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    let other = fields::to_date_time(vm, arg(a, 0), Value::UNDEFINED)?;
    Ok(Value::from_bool(dt == other))
}

fn date_time_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let show = show_calendar(vm, o)?;
    let digits = fractional_digits(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::Trunc)?;
    let smallest = unit_option(vm, o, "smallestUnit", false)?;
    validate_unit(smallest, Group::Time, false)?;
    if smallest == UnitOption::Unit(Unit::Hour) {
        return Err(range("smallestUnit must be a time unit other than hour"));
    }
    let (precision, unit, increment) = seconds_precision(smallest, digits);
    let r = check_date_time(round_date_time(dt, increment, unit, mode))?;
    Ok(string(vm, &format!("{}{}", iso::date_time_string(r, precision), calendar_annotation(show))))
}

fn date_time_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    Ok(string(vm, &iso::date_time_string(dt, iso::Precision::Auto)))
}

fn date_time_to_zoned(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    let z = to_time_zone(vm, arg(a, 0))?;
    let o = options_object(vm, arg(a, 1))?;
    let d = disambiguation(vm, o)?;
    let ns = epoch_ns_for(vm, z, dt, d)?;
    super::zoned::create_zoned(vm, ns, z)
}

fn date_time_to_plain_date(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    create_date(vm, dt.date)
}

fn date_time_to_plain_time(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let dt = this_date_time(this)?;
    Ok(create_time(vm, dt.time))
}

fn date_time_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let dt = fields::to_date_time(vm, arg(a, 0), arg(a, 1))?;
    create_date_time(vm, dt)
}

fn date_time_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let one = fields::to_date_time(vm, arg(a, 0), Value::UNDEFINED)?;
    let two = fields::to_date_time(vm, arg(a, 1), Value::UNDEFINED)?;
    Ok(Value::from_i32(one.cmp(&two) as i32))
}

const DT: usize = Kind::DateTime as usize;

pub(crate) fn date_time_spec() -> ClassSpec {
    ClassSpec {
        name: "PlainDateTime",
        kind: Kind::DateTime,
        ctor: date_time_ctor,
        length: 3,
        statics: &[("from", date_time_from as NativeFn, 1), ("compare", date_time_compare, 2)],
        getters: &[
            ("calendarId", date_getter::<DT, 15>),
            ("era", date_getter::<DT, 13>),
            ("eraYear", date_getter::<DT, 14>),
            ("year", date_getter::<DT, 0>),
            ("month", date_getter::<DT, 1>),
            ("monthCode", date_getter::<DT, 2>),
            ("day", date_getter::<DT, 3>),
            ("hour", time_getter::<DT, 0>),
            ("minute", time_getter::<DT, 1>),
            ("second", time_getter::<DT, 2>),
            ("millisecond", time_getter::<DT, 3>),
            ("microsecond", time_getter::<DT, 4>),
            ("nanosecond", time_getter::<DT, 5>),
            ("dayOfWeek", date_getter::<DT, 4>),
            ("dayOfYear", date_getter::<DT, 5>),
            ("weekOfYear", date_getter::<DT, 6>),
            ("yearOfWeek", date_getter::<DT, 7>),
            ("daysInWeek", date_getter::<DT, 8>),
            ("daysInMonth", date_getter::<DT, 9>),
            ("daysInYear", date_getter::<DT, 10>),
            ("monthsInYear", date_getter::<DT, 11>),
            ("inLeapYear", date_getter::<DT, 12>),
        ],
        methods: &[
            ("with", date_time_with as NativeFn, 1),
            ("withPlainTime", date_time_with_plain_time, 0),
            ("withCalendar", date_time_with_calendar, 1),
            ("add", date_time_add, 1),
            ("subtract", date_time_subtract, 1),
            ("until", date_time_until, 1),
            ("since", date_time_since, 1),
            ("round", date_time_round, 1),
            ("equals", date_time_equals, 1),
            ("toString", date_time_to_string, 0),
            ("toJSON", date_time_to_json, 0),
            ("toLocaleString", date_time_to_json, 0),
            ("toZonedDateTime", date_time_to_zoned, 1),
            ("toPlainDate", date_time_to_plain_date, 0),
            ("toPlainTime", date_time_to_plain_time, 0),
        ],
    }
}

// ---- Temporal.PlainYearMonth ----

fn this_year_month(this: Value) -> Result<IsoDate> {
    match this_slots(this, Kind::YearMonth)? {
        Slots::YearMonth(d) => Ok(d),
        _ => unreachable!(),
    }
}

pub(crate) fn create_year_month(vm: &mut Vm, d: IsoDate) -> Result<Value> {
    Ok(create(vm, Slots::YearMonth(check_year_month(d)?)))
}

fn year_month_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.PlainYearMonth requires 'new'"));
    }
    let y = to_integer(vm, arg(a, 0))?;
    let m = to_integer(vm, arg(a, 1))?;
    calendar_arg(arg(a, 2))?;
    let day = if arg(a, 3).is_undefined() { 1.0 } else { to_integer(vm, arg(a, 3))? };
    let d = reject_iso_date(y, m, day)?;
    create_year_month(vm, d)
}

fn year_month_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_year_month(this)?;
    let item = arg(a, 0);
    reject_temporal_like(vm, item)?;
    let base = fields::date_fields(d, true, false);
    let partial = fields::prepare(vm, item, &[Field::Year, Field::Month, Field::MonthCode], Required::Partial)?;
    let merged = fields::merge(&base, &partial);
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let r = fields::year_month_from_fields(&merged, ov)?;
    create_year_month(vm, r)
}

/// AddDurationToYearMonth.
fn year_month_add_impl(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let d = this_year_month(this)?;
    let mut dur = duration::to_duration(vm, arg(a, 0))?;
    if subtract {
        dur = duration::negated(&dur);
    }
    let i = duration::to_internal(&dur);
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    if i.date[2] != 0.0 || i.date[3] != 0.0 || i.time != 0 {
        return Err(range("only years and months can be added to Temporal.PlainYearMonth"));
    }
    let start = fields::date_from_fields(&fields::Fields { day: Some(1.0), ..fields::date_fields(d, true, false) }, Overflow::Constrain)?;
    let added = fields::date_add(start, &i.date, ov)?;
    let f = fields::date_fields(added, true, false);
    let r = fields::year_month_from_fields(&f, ov)?;
    create_year_month(vm, r)
}

fn year_month_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    year_month_add_impl(vm, this, a, false)
}

fn year_month_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    year_month_add_impl(vm, this, a, true)
}

/// DifferenceTemporalPlainYearMonth.
fn year_month_difference(vm: &mut Vm, this: Value, a: &[Value], since: bool) -> Result<Value> {
    let d = this_year_month(this)?;
    let other = fields::to_year_month(vm, arg(a, 0), Value::UNDEFINED)?;
    let o = options_object(vm, arg(a, 1))?;
    let s = duration::difference_settings(vm, since, o, Group::Date, &[Unit::Week, Unit::Day], Unit::Month, Unit::Year)?;
    if d == other {
        return Ok(duration::create_duration(vm, [0.0; 10]));
    }
    let this_date = IsoDate { day: 1, ..d };
    let other_date = IsoDate { day: 1, ..other };
    check_date(this_date)?;
    check_date(other_date)?;
    let diff = fields::date_until(this_date, other_date, s.largest);
    let mut internal = duration::Internal { date: duration::adjust(&diff, 0.0, Some(0.0), None)?, time: 0 };
    if s.smallest != Unit::Month || s.increment != 1 {
        let start = DateTime { date: this_date, time: Time::MIDNIGHT };
        let end = DateTime { date: other_date, time: Time::MIDNIGHT };
        internal = duration::round_relative_plain(vm, internal, start, end, s.largest, s.increment, s.smallest, s.mode)?;
    }
    let r = duration::from_internal(&internal, Unit::Day)?;
    Ok(since_until(vm, r, since))
}

fn year_month_until(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    year_month_difference(vm, this, a, false)
}

fn year_month_since(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    year_month_difference(vm, this, a, true)
}

fn year_month_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_year_month(this)?;
    let other = fields::to_year_month(vm, arg(a, 0), Value::UNDEFINED)?;
    Ok(Value::from_bool(d == other))
}

fn year_month_string(d: IsoDate, show: usize) -> String {
    let mut s = format!("{}-{:02}", iso::year_string(d.year), d.month);
    if show == 1 || show == 3 {
        s += &format!("-{:02}", d.day);
    }
    s + calendar_annotation(show)
}

fn year_month_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_year_month(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let show = show_calendar(vm, o)?;
    Ok(string(vm, &year_month_string(d, show)))
}

fn year_month_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_year_month(this)?;
    Ok(string(vm, &year_month_string(d, 0)))
}

fn year_month_to_plain_date(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_year_month(this)?;
    let item = arg(a, 0);
    if !is_object(item) {
        return Err(type_err("argument should be an object"));
    }
    let base = fields::date_fields(d, true, false);
    let input = fields::prepare(vm, item, &[Field::Day], Required::Some(&[]))?;
    let merged = fields::merge(&base, &input);
    let r = fields::date_from_fields(&merged, Overflow::Constrain)?;
    create_date(vm, r)
}

fn year_month_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let d = fields::to_year_month(vm, arg(a, 0), arg(a, 1))?;
    create_year_month(vm, d)
}

fn year_month_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let one = fields::to_year_month(vm, arg(a, 0), Value::UNDEFINED)?;
    let two = fields::to_year_month(vm, arg(a, 1), Value::UNDEFINED)?;
    Ok(Value::from_i32(one.cmp(&two) as i32))
}

const YM: usize = Kind::YearMonth as usize;

pub(crate) fn year_month_spec() -> ClassSpec {
    ClassSpec {
        name: "PlainYearMonth",
        kind: Kind::YearMonth,
        ctor: year_month_ctor,
        length: 2,
        statics: &[("from", year_month_from as NativeFn, 1), ("compare", year_month_compare, 2)],
        getters: &[
            ("calendarId", date_getter::<YM, 15>),
            ("era", date_getter::<YM, 13>),
            ("eraYear", date_getter::<YM, 14>),
            ("year", date_getter::<YM, 0>),
            ("month", date_getter::<YM, 1>),
            ("monthCode", date_getter::<YM, 2>),
            ("daysInYear", date_getter::<YM, 10>),
            ("daysInMonth", date_getter::<YM, 9>),
            ("monthsInYear", date_getter::<YM, 11>),
            ("inLeapYear", date_getter::<YM, 12>),
        ],
        methods: &[
            ("with", year_month_with as NativeFn, 1),
            ("add", year_month_add, 1),
            ("subtract", year_month_subtract, 1),
            ("until", year_month_until, 1),
            ("since", year_month_since, 1),
            ("equals", year_month_equals, 1),
            ("toString", year_month_to_string, 0),
            ("toJSON", year_month_to_json, 0),
            ("toLocaleString", year_month_to_json, 0),
            ("toPlainDate", year_month_to_plain_date, 1),
        ],
    }
}

// ---- Temporal.PlainMonthDay ----

fn this_month_day(this: Value) -> Result<IsoDate> {
    match this_slots(this, Kind::MonthDay)? {
        Slots::MonthDay(d) => Ok(d),
        _ => unreachable!(),
    }
}

pub(crate) fn create_month_day(vm: &mut Vm, d: IsoDate) -> Result<Value> {
    Ok(create(vm, Slots::MonthDay(check_date(d)?)))
}

fn month_day_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.PlainMonthDay requires 'new'"));
    }
    let m = to_integer(vm, arg(a, 0))?;
    let d = to_integer(vm, arg(a, 1))?;
    calendar_arg(arg(a, 2))?;
    let y = if arg(a, 3).is_undefined() { 1972.0 } else { to_integer(vm, arg(a, 3))? };
    let date = reject_iso_date(y, m, d)?;
    create_month_day(vm, date)
}

fn month_day_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_month_day(this)?;
    let item = arg(a, 0);
    reject_temporal_like(vm, item)?;
    let base = fields::date_fields(d, false, true);
    let partial = fields::prepare(vm, item, fields::DATE_FIELDS, Required::Partial)?;
    let merged = fields::merge(&base, &partial);
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let r = fields::month_day_from_fields(&merged, ov)?;
    create_month_day(vm, r)
}

fn month_day_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_month_day(this)?;
    let other = fields::to_month_day(vm, arg(a, 0), Value::UNDEFINED)?;
    Ok(Value::from_bool(d == other))
}

fn month_day_string(d: IsoDate, show: usize) -> String {
    let mut s = format!("{:02}-{:02}", d.month, d.day);
    if show == 1 || show == 3 {
        s = format!("{}-{s}", iso::year_string(d.year));
    }
    s + calendar_annotation(show)
}

fn month_day_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_month_day(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let show = show_calendar(vm, o)?;
    Ok(string(vm, &month_day_string(d, show)))
}

fn month_day_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_month_day(this)?;
    Ok(string(vm, &month_day_string(d, 0)))
}

fn month_day_to_plain_date(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_month_day(this)?;
    let item = arg(a, 0);
    if !is_object(item) {
        return Err(type_err("argument should be an object"));
    }
    let base = fields::date_fields(d, false, true);
    let input = fields::prepare(vm, item, &[Field::Year], Required::Some(&[]))?;
    let merged = fields::merge(&base, &input);
    let r = fields::date_from_fields(&merged, Overflow::Constrain)?;
    create_date(vm, r)
}

fn month_day_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let d = fields::to_month_day(vm, arg(a, 0), arg(a, 1))?;
    create_month_day(vm, d)
}

const MD: usize = Kind::MonthDay as usize;

pub(crate) fn month_day_spec() -> ClassSpec {
    ClassSpec {
        name: "PlainMonthDay",
        kind: Kind::MonthDay,
        ctor: month_day_ctor,
        length: 2,
        statics: &[("from", month_day_from as NativeFn, 1)],
        getters: &[("calendarId", date_getter::<MD, 15>), ("monthCode", date_getter::<MD, 2>), ("day", date_getter::<MD, 3>)],
        methods: &[
            ("with", month_day_with as NativeFn, 1),
            ("equals", month_day_equals, 1),
            ("toString", month_day_to_string, 0),
            ("toJSON", month_day_to_json, 0),
            ("toLocaleString", month_day_to_json, 0),
            ("toPlainDate", month_day_to_plain_date, 1),
        ],
    }
}
