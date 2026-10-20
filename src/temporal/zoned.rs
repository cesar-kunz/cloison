//! Temporal.Instant, Temporal.ZonedDateTime, Temporal.Now, and
//! `Date.prototype.toTemporalInstant`.

use super::duration;
use super::fields::{self, Field, Required};
use super::iso::{self, DateTime, RoundingMode, Unit};
use super::plain::{self, date_getter, time_getter};
use super::*;

/// ToBigInt of `v`, as an exact time in range (a RangeError if not).
fn to_epoch_ns(vm: &mut Vm, v: Value) -> Result<i128> {
    let b = crate::bigint::to_bigint(vm, v)?;
    let ns: Option<i128> = b.to_string_radix(10).parse().ok();
    match ns {
        Some(ns) if iso::valid_epoch_ns(ns) => Ok(ns),
        _ => Err(range("epoch nanoseconds are outside of supported range")),
    }
}

fn epoch_ms(ns: i128) -> Value {
    Value::number(ns.div_euclid(1_000_000) as f64)
}

/// The current exact time.
fn system_ns() -> i128 {
    let d = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as i128).unwrap_or(0);
    d.clamp(iso::NS_MIN, iso::NS_MAX)
}

// ---- Temporal.Instant ----

fn this_instant(this: Value) -> Result<i128> {
    match this_slots(this, Kind::Instant)? {
        Slots::Instant(ns) => Ok(ns),
        _ => unreachable!(),
    }
}

pub(crate) fn create_instant(vm: &mut Vm, ns: i128) -> Result<Value> {
    Ok(create(vm, Slots::Instant(check_epoch_ns(ns)?)))
}

fn instant_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.Instant requires 'new'"));
    }
    let ns = to_epoch_ns(vm, arg(a, 0))?;
    create_instant(vm, ns)
}

fn instant_epoch_ms(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(epoch_ms(this_instant(this)?))
}

fn instant_epoch_ns(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let ns = this_instant(this)?;
    Ok(bigint_of(vm, ns))
}

/// AddDurationToInstant.
fn instant_add_impl(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let ns = this_instant(this)?;
    let mut d = duration::to_duration(vm, arg(a, 0))?;
    if subtract {
        d = duration::negated(&d);
    }
    let largest = duration::default_largest_unit(&d);
    if largest.is_date() {
        return Err(range(format!("duration field {} not supported by Temporal.Instant", largest.name())));
    }
    let r = duration::add_instant(ns, duration::to_internal_24h(&d).time)?;
    create_instant(vm, r)
}

fn instant_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    instant_add_impl(vm, this, a, false)
}

fn instant_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    instant_add_impl(vm, this, a, true)
}

/// DifferenceTemporalInstant.
fn instant_difference(vm: &mut Vm, this: Value, a: &[Value], since: bool) -> Result<Value> {
    let ns = this_instant(this)?;
    let other = fields::to_instant(vm, arg(a, 0))?;
    let o = options_object(vm, arg(a, 1))?;
    let s = duration::difference_settings(vm, since, o, Group::Time, &[], Unit::Nanosecond, Unit::Second)?;
    let i = duration::difference_instant(ns, other, s.increment, s.smallest, s.mode)?;
    let mut r = duration::from_internal(&i, s.largest)?;
    if since {
        r = duration::negated(&r);
    }
    Ok(duration::create_duration(vm, r))
}

fn instant_until(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    instant_difference(vm, this, a, false)
}

fn instant_since(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    instant_difference(vm, this, a, true)
}

/// `round(roundTo)`'s options: a string the smallest unit.
fn round_options(vm: &mut Vm, v: Value, name: &str) -> Result<Value> {
    if v.is_undefined() {
        return Err(type_err("options parameter is required"));
    }
    if string_units(v).is_some() {
        let o = object_value(vm.new_object(Value::NULL));
        let a = vm.intern(name);
        vm.add_property(as_object(o), a, v);
        return Ok(o);
    }
    options_object(vm, v)
}

fn instant_round(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let ns = this_instant(this)?;
    let o = round_options(vm, arg(a, 0), "smallestUnit")?;
    let increment = rounding_increment(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::HalfExpand)?;
    let smallest = unit_option(vm, o, "smallestUnit", true)?;
    validate_unit(smallest, Group::Time, false)?;
    let UnitOption::Unit(unit) = smallest else { return Err(range("smallestUnit is required")) };
    let max = (iso::NS_PER_DAY / unit.nanos()) as i64;
    validate_increment(increment, max, true)?;
    let r = iso::round_as_if_positive(ns, unit.nanos() * increment as i128, mode);
    create_instant(vm, r)
}

fn instant_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let ns = this_instant(this)?;
    let other = fields::to_instant(vm, arg(a, 0))?;
    Ok(Value::from_bool(ns == other))
}

/// TemporalInstantToString (in zone `z`, UTC with `Z` if none).
fn instant_string(vm: &Vm, ns: i128, z: Option<u32>, precision: iso::Precision) -> String {
    match z {
        None => format!("{}Z", iso::date_time_string(iso::from_epoch_ns(ns), precision)),
        Some(z) => {
            let zn = zone(vm, z);
            let off = zn.offset_ns(ns);
            format!("{}{}", iso::date_time_string(zn.date_time_at(ns), precision), iso::offset_string_rounded(off))
        }
    }
}

fn instant_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let ns = this_instant(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let digits = fractional_digits(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::Trunc)?;
    let smallest = unit_option(vm, o, "smallestUnit", false)?;
    let tz = get(vm, o, "timeZone")?;
    validate_unit(smallest, Group::Time, false)?;
    if smallest == UnitOption::Unit(Unit::Hour) {
        return Err(range("smallestUnit must be a time unit other than hour"));
    }
    let z = if tz.is_undefined() { None } else { Some(to_time_zone(vm, tz)?) };
    let (precision, unit, increment) = seconds_precision(smallest, digits);
    let r = check_epoch_ns(iso::round_as_if_positive(ns, unit.nanos() * increment as i128, mode))?;
    let s = instant_string(vm, r, z, precision);
    Ok(string(vm, &s))
}

fn instant_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let ns = this_instant(this)?;
    let s = instant_string(vm, ns, None, iso::Precision::Auto);
    Ok(string(vm, &s))
}

fn instant_to_zoned(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let ns = this_instant(this)?;
    let z = to_time_zone(vm, arg(a, 0))?;
    create_zoned(vm, ns, z)
}

fn instant_from_epoch_ms(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let ms = number(vm, arg(a, 0))?;
    if !ms.is_finite() || ms.trunc() != ms {
        return Err(range("epoch milliseconds must be an integer"));
    }
    if ms.abs() > 1e16 {
        return Err(range("epoch milliseconds are outside of supported range"));
    }
    create_instant(vm, ms as i128 * 1_000_000)
}

fn instant_from_epoch_ns(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let ns = to_epoch_ns(vm, arg(a, 0))?;
    create_instant(vm, ns)
}

fn instant_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let ns = fields::to_instant(vm, arg(a, 0))?;
    create_instant(vm, ns)
}

fn instant_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let one = fields::to_instant(vm, arg(a, 0))?;
    let two = fields::to_instant(vm, arg(a, 1))?;
    Ok(Value::from_i32(one.cmp(&two) as i32))
}

pub(crate) fn instant_spec() -> ClassSpec {
    ClassSpec {
        name: "Instant",
        kind: Kind::Instant,
        ctor: instant_ctor,
        length: 1,
        statics: &[
            ("from", instant_from as NativeFn, 1),
            ("fromEpochMilliseconds", instant_from_epoch_ms, 1),
            ("fromEpochNanoseconds", instant_from_epoch_ns, 1),
            ("compare", instant_compare, 2),
        ],
        getters: &[("epochMilliseconds", instant_epoch_ms), ("epochNanoseconds", instant_epoch_ns)],
        methods: &[
            ("add", instant_add as NativeFn, 1),
            ("subtract", instant_subtract, 1),
            ("until", instant_until, 1),
            ("since", instant_since, 1),
            ("round", instant_round, 1),
            ("equals", instant_equals, 1),
            ("toString", instant_to_string, 0),
            ("toJSON", instant_to_json, 0),
            ("toLocaleString", instant_to_json, 0),
            ("toZonedDateTimeISO", instant_to_zoned, 1),
        ],
    }
}

// ---- Temporal.ZonedDateTime ----

fn this_zoned(this: Value) -> Result<(i128, u32)> {
    match this_slots(this, Kind::Zoned)? {
        Slots::Zoned(ns, z) => Ok((ns, z)),
        _ => unreachable!(),
    }
}

pub(crate) fn create_zoned(vm: &mut Vm, ns: i128, z: u32) -> Result<Value> {
    Ok(create(vm, Slots::Zoned(check_epoch_ns(ns)?, z)))
}

fn zoned_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.ZonedDateTime requires 'new'"));
    }
    let b = crate::bigint::to_bigint(vm, arg(a, 0))?;
    let id = require_string(arg(a, 1))?;
    let z = time_zone_from_identifier(vm, &id)?;
    let c = arg(a, 2);
    if !c.is_undefined() {
        canonicalize_calendar(&require_string(c)?)?;
    }
    let ns: Option<i128> = b.to_string_radix(10).parse().ok();
    match ns {
        Some(ns) => create_zoned(vm, ns, z),
        None => Err(range("epoch nanoseconds are outside of supported range")),
    }
}

fn zoned_time_zone_id(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (_, z) = this_zoned(this)?;
    let id = zone(vm, z).id.clone();
    Ok(string(vm, &id))
}

fn zoned_epoch_ms(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(epoch_ms(this_zoned(this)?.0))
}

fn zoned_epoch_ns(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, _) = this_zoned(this)?;
    Ok(bigint_of(vm, ns))
}

fn zoned_hours_in_day(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let today = zone(vm, z).date_time_at(ns).date;
    let tomorrow = iso::add_days(today, 1);
    let a = start_of_day(vm, z, today)?;
    let b = start_of_day(vm, z, tomorrow)?;
    Ok(Value::number(iso::fdiv(b - a, Unit::Hour.nanos())))
}

fn zoned_offset_ns(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    Ok(Value::number(zone(vm, z).offset_ns(ns) as f64))
}

fn zoned_offset(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let s = iso::offset_string(zone(vm, z).offset_ns(ns));
    Ok(string(vm, &s))
}

fn zoned_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let item = arg(a, 0);
    plain::reject_temporal_like(vm, item)?;
    let off = zone(vm, z).offset_ns(ns);
    let dt = zone(vm, z).date_time_at(ns);
    let mut base = fields::date_fields(dt.date, true, true);
    base.time = dt.time.fields().map(|v| Some(v as f64));
    base.offset = Some(iso::offset_string(off));
    let which: Vec<Field> = fields::DATE_FIELDS.iter().chain(fields::TIME_FIELDS).copied().chain([Field::Offset]).collect();
    let partial = fields::prepare(vm, item, &which, Required::Partial)?;
    let merged = fields::merge(&base, &partial);
    let o = options_object(vm, arg(a, 1))?;
    let d = disambiguation(vm, o)?;
    let offset_opt = offset_option(vm, o, OffsetOption::Prefer)?;
    let ov = overflow(vm, o)?;
    let date = fields::date_from_fields(&merged, ov)?;
    let time = fields::time_from_fields(&merged, ov)?;
    let new_off = parse::parse_offset(merged.offset.as_deref().unwrap()).map_err(parse_err)?.ns;
    let r = interpret_offset(vm, date, Some(time), OffsetBehaviour::Option, new_off, z, d, offset_opt, false)?;
    create_zoned(vm, r, z)
}

fn zoned_with_plain_time(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let date = zone(vm, z).date_time_at(ns).date;
    let t = arg(a, 0);
    let r = if t.is_undefined() {
        start_of_day(vm, z, date)?
    } else {
        let time = fields::to_time(vm, t, Value::UNDEFINED)?;
        epoch_ns_for(vm, z, DateTime { date, time }, Disambiguation::Compatible)?
    };
    create_zoned(vm, r, z)
}

fn zoned_with_time_zone(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, _) = this_zoned(this)?;
    let z = to_time_zone(vm, arg(a, 0))?;
    create_zoned(vm, ns, z)
}

fn zoned_with_calendar(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    to_calendar(vm, arg(a, 0))?;
    create_zoned(vm, ns, z)
}

/// AddDurationToZonedDateTime.
fn zoned_add_impl(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let mut d = duration::to_duration(vm, arg(a, 0))?;
    if subtract {
        d = duration::negated(&d);
    }
    let o = options_object(vm, arg(a, 1))?;
    let ov = overflow(vm, o)?;
    let r = duration::add_zoned(vm, ns, z, &duration::to_internal(&d), ov)?;
    create_zoned(vm, r, z)
}

fn zoned_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    zoned_add_impl(vm, this, a, false)
}

fn zoned_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    zoned_add_impl(vm, this, a, true)
}

/// DifferenceTemporalZonedDateTime.
fn zoned_difference(vm: &mut Vm, this: Value, a: &[Value], since: bool) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let (other, oz) = fields::to_zoned(vm, arg(a, 0), Value::UNDEFINED)?;
    let o = options_object(vm, arg(a, 1))?;
    let s = duration::difference_settings(vm, since, o, Group::DateTime, &[], Unit::Nanosecond, Unit::Hour)?;
    let mut r = if !s.largest.is_date() {
        let i = duration::difference_instant(ns, other, s.increment, s.smallest, s.mode)?;
        duration::from_internal(&i, s.largest)?
    } else {
        if !zone(vm, z).equals(zone(vm, oz)) {
            return Err(range("when calculating difference between time zones, largestUnit must be 'hours' or smaller"));
        }
        if ns == other {
            return Ok(duration::create_duration(vm, [0.0; 10]));
        }
        let i = duration::difference_zoned_rounded(vm, ns, other, z, s.largest, s.increment, s.smallest, s.mode)?;
        duration::from_internal(&i, Unit::Hour)?
    };
    if since {
        r = duration::negated(&r);
    }
    Ok(duration::create_duration(vm, r))
}

fn zoned_until(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    zoned_difference(vm, this, a, false)
}

fn zoned_since(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    zoned_difference(vm, this, a, true)
}

fn zoned_round(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let o = round_options(vm, arg(a, 0), "smallestUnit")?;
    let increment = rounding_increment(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::HalfExpand)?;
    let smallest = unit_option(vm, o, "smallestUnit", true)?;
    if smallest != UnitOption::Unit(Unit::Day) {
        validate_unit(smallest, Group::Time, false)?;
    }
    let UnitOption::Unit(unit) = smallest else { return Err(range("smallestUnit is required")) };
    let max = duration::max_increment(unit).unwrap_or(1);
    validate_increment(increment, max, max == 1)?;
    if unit == Unit::Nanosecond && increment == 1 {
        return create_zoned(vm, ns, z);
    }
    let dt = zone(vm, z).date_time_at(ns);
    let r = if unit == Unit::Day {
        let start = start_of_day(vm, z, dt.date)?;
        let end = start_of_day(vm, z, iso::add_days(dt.date, 1))?;
        let this_ns = if ns >= end { end - 1 } else { ns };
        let length = end - start;
        let progress = this_ns - start;
        start + iso::round_to_increment(progress, length, mode)
    } else {
        let rounded = plain::round_date_time(dt, increment, unit, mode);
        let off = zone(vm, z).offset_ns(ns);
        interpret_offset(
            vm,
            rounded.date,
            Some(rounded.time),
            OffsetBehaviour::Option,
            off,
            z,
            Disambiguation::Compatible,
            OffsetOption::Prefer,
            false,
        )?
    };
    create_zoned(vm, r, z)
}

fn zoned_equals(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let (other, oz) = fields::to_zoned(vm, arg(a, 0), Value::UNDEFINED)?;
    Ok(Value::from_bool(ns == other && zone(vm, z).equals(zone(vm, oz))))
}

/// TemporalZonedDateTimeToString.
#[allow(clippy::too_many_arguments)]
fn zoned_string(vm: &Vm, ns: i128, z: u32, precision: iso::Precision, show_calendar: usize, show_tz: usize, show_offset: bool, rounding: Option<(i64, Unit, RoundingMode)>) -> String {
    let ns = match rounding {
        Some((increment, unit, mode)) => iso::round_as_if_positive(ns, unit.nanos() * increment as i128, mode),
        None => ns,
    };
    let zn = zone(vm, z);
    let off = zn.offset_ns(ns);
    let mut s = iso::date_time_string(zn.date_time_at(ns), precision);
    if show_offset {
        s += &iso::offset_string_rounded(off);
    }
    match show_tz {
        0 => s += &format!("[{}]", zn.id),
        2 => s += &format!("[!{}]", zn.id),
        _ => {}
    }
    s + calendar_annotation(show_calendar)
}

fn zoned_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let show_calendar = show_calendar(vm, o)?;
    let digits = fractional_digits(vm, o)?;
    let show_offset = option(vm, o, "offset", &["auto", "never"], Some(0))? == 0;
    let mode = rounding_mode(vm, o, RoundingMode::Trunc)?;
    let smallest = unit_option(vm, o, "smallestUnit", false)?;
    let show_tz = option(vm, o, "timeZoneName", &["auto", "never", "critical"], Some(0))?;
    validate_unit(smallest, Group::Time, false)?;
    if smallest == UnitOption::Unit(Unit::Hour) {
        return Err(range("smallestUnit must be a time unit other than hour"));
    }
    let (precision, unit, increment) = seconds_precision(smallest, digits);
    let s = zoned_string(vm, ns, z, precision, show_calendar, show_tz, show_offset, Some((increment, unit, mode)));
    Ok(string(vm, &s))
}

fn zoned_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let s = zoned_string(vm, ns, z, iso::Precision::Auto, 0, 0, true, None);
    Ok(string(vm, &s))
}

fn zoned_start_of_day(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let date = zone(vm, z).date_time_at(ns).date;
    let r = start_of_day(vm, z, date)?;
    create_zoned(vm, r, z)
}

fn zoned_transition(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let o = round_options(vm, arg(a, 0), "direction")?;
    let next = option(vm, o, "direction", &["next", "previous"], None)? == 0;
    let zn = zone(vm, z);
    let t = if next { zn.next_transition(ns) } else { zn.previous_transition(ns) };
    match t {
        Some(t) => create_zoned(vm, t, z),
        None => Ok(Value::NULL),
    }
}

fn zoned_to_instant(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, _) = this_zoned(this)?;
    create_instant(vm, ns)
}

fn zoned_to_plain_date(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let d = zone(vm, z).date_time_at(ns).date;
    plain::create_date(vm, d)
}

fn zoned_to_plain_time(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let t = zone(vm, z).date_time_at(ns).time;
    Ok(plain::create_time(vm, t))
}

fn zoned_to_plain_date_time(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let (ns, z) = this_zoned(this)?;
    let dt = zone(vm, z).date_time_at(ns);
    plain::create_date_time(vm, dt)
}

fn zoned_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (ns, z) = fields::to_zoned(vm, arg(a, 0), arg(a, 1))?;
    create_zoned(vm, ns, z)
}

fn zoned_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let (one, _) = fields::to_zoned(vm, arg(a, 0), Value::UNDEFINED)?;
    let (two, _) = fields::to_zoned(vm, arg(a, 1), Value::UNDEFINED)?;
    Ok(Value::from_i32(one.cmp(&two) as i32))
}

const Z: usize = Kind::Zoned as usize;

pub(crate) fn zoned_spec() -> ClassSpec {
    ClassSpec {
        name: "ZonedDateTime",
        kind: Kind::Zoned,
        ctor: zoned_ctor,
        length: 2,
        statics: &[("from", zoned_from as NativeFn, 1), ("compare", zoned_compare, 2)],
        getters: &[
            ("calendarId", date_getter::<Z, 15>),
            ("timeZoneId", zoned_time_zone_id),
            ("era", date_getter::<Z, 13>),
            ("eraYear", date_getter::<Z, 14>),
            ("year", date_getter::<Z, 0>),
            ("month", date_getter::<Z, 1>),
            ("monthCode", date_getter::<Z, 2>),
            ("day", date_getter::<Z, 3>),
            ("hour", time_getter::<Z, 0>),
            ("minute", time_getter::<Z, 1>),
            ("second", time_getter::<Z, 2>),
            ("millisecond", time_getter::<Z, 3>),
            ("microsecond", time_getter::<Z, 4>),
            ("nanosecond", time_getter::<Z, 5>),
            ("epochMilliseconds", zoned_epoch_ms),
            ("epochNanoseconds", zoned_epoch_ns),
            ("dayOfWeek", date_getter::<Z, 4>),
            ("dayOfYear", date_getter::<Z, 5>),
            ("weekOfYear", date_getter::<Z, 6>),
            ("yearOfWeek", date_getter::<Z, 7>),
            ("hoursInDay", zoned_hours_in_day),
            ("daysInWeek", date_getter::<Z, 8>),
            ("daysInMonth", date_getter::<Z, 9>),
            ("daysInYear", date_getter::<Z, 10>),
            ("monthsInYear", date_getter::<Z, 11>),
            ("inLeapYear", date_getter::<Z, 12>),
            ("offsetNanoseconds", zoned_offset_ns),
            ("offset", zoned_offset),
        ],
        methods: &[
            ("with", zoned_with as NativeFn, 1),
            ("withPlainTime", zoned_with_plain_time, 0),
            ("withTimeZone", zoned_with_time_zone, 1),
            ("withCalendar", zoned_with_calendar, 1),
            ("add", zoned_add, 1),
            ("subtract", zoned_subtract, 1),
            ("until", zoned_until, 1),
            ("since", zoned_since, 1),
            ("round", zoned_round, 1),
            ("equals", zoned_equals, 1),
            ("toString", zoned_to_string, 0),
            ("toJSON", zoned_to_json, 0),
            ("toLocaleString", zoned_to_json, 0),
            ("startOfDay", zoned_start_of_day, 0),
            ("getTimeZoneTransition", zoned_transition, 1),
            ("toInstant", zoned_to_instant, 0),
            ("toPlainDate", zoned_to_plain_date, 0),
            ("toPlainTime", zoned_to_plain_time, 0),
            ("toPlainDateTime", zoned_to_plain_date_time, 0),
        ],
    }
}

// ---- Temporal.Now ----

/// The time zone argument of a Now function: the system's if undefined.
fn now_zone(vm: &mut Vm, v: Value) -> Result<u32> {
    if v.is_undefined() { Ok(system_time_zone(vm)) } else { to_time_zone(vm, v) }
}

fn now_instant(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    create_instant(vm, system_ns())
}

fn now_time_zone_id(vm: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    let z = system_time_zone(vm);
    let id = zone(vm, z).id.clone();
    Ok(string(vm, &id))
}

fn now_zoned(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let z = now_zone(vm, arg(a, 0))?;
    create_zoned(vm, system_ns(), z)
}

fn now_date_time(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let z = now_zone(vm, arg(a, 0))?;
    let dt = zone(vm, z).date_time_at(system_ns());
    plain::create_date_time(vm, dt)
}

fn now_date(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let z = now_zone(vm, arg(a, 0))?;
    let dt = zone(vm, z).date_time_at(system_ns());
    plain::create_date(vm, dt.date)
}

fn now_time(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let z = now_zone(vm, arg(a, 0))?;
    let dt = zone(vm, z).date_time_at(system_ns());
    Ok(plain::create_time(vm, dt.time))
}

/// `Date.prototype.toTemporalInstant()`.
fn to_temporal_instant(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let Some(t) = crate::date::date_value(this) else {
        return Err(type_err("Date.prototype.toTemporalInstant called on an incompatible receiver"));
    };
    if !t.is_finite() {
        return Err(range("invalid date"));
    }
    create_instant(vm, t as i128 * 1_000_000)
}

pub(crate) fn install_now(vm: &mut Vm, temporal: *mut Object) {
    let op = object_value(vm.object_prototype);
    let now = vm.new_object(op);
    let a = vm.intern("Now");
    vm.add_property_with(temporal, a, object_value(now), crate::object::DONT_ENUM);
    vm.define_natives(
        now,
        &[
            ("instant", now_instant as NativeFn, 0),
            ("plainDateTimeISO", now_date_time, 0),
            ("plainDateISO", now_date, 0),
            ("plainTimeISO", now_time, 0),
            ("timeZoneId", now_time_zone_id, 0),
            ("zonedDateTimeISO", now_zoned, 0),
        ],
    );
    crate::symbol::to_string_tag(vm, now, "Temporal.Now");
    let dp = vm.date_prototype;
    vm.define_natives(dp, &[("toTemporalInstant", to_temporal_instant as NativeFn, 0)]);
}
