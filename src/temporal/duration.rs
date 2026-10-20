//! Temporal.Duration, and the arithmetic of durations the other classes
//! share: internal duration records (a date part and a time duration in
//! nanoseconds), differences of dates and times, and rounding relative to
//! a starting point (RoundRelativeDuration and its nudges).

use super::fields;
use super::iso::{self, DateTime, IsoDate, RoundingMode, TIME_DURATION_MAX, Time, Unit};
use super::*;

/// A duration's fields: years, months, weeks, days, hours, minutes,
/// seconds, milliseconds, microseconds, nanoseconds.
pub(crate) type Dur = [f64; 10];

/// An internal duration record: `[years, months, weeks, days]`, and a time
/// duration in nanoseconds.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Internal {
    pub date: [f64; 4],
    pub time: i128,
}

pub(crate) const ZERO_DATE: [f64; 4] = [0.0; 4];

fn sign_of(v: f64) -> i64 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

/// DurationSign.
pub(crate) fn duration_sign(d: &Dur) -> i64 {
    d.iter().map(|&v| sign_of(v)).find(|&s| s != 0).unwrap_or(0)
}

fn date_sign(d: &[f64; 4]) -> i64 {
    d.iter().map(|&v| sign_of(v)).find(|&s| s != 0).unwrap_or(0)
}

fn internal_sign(d: &Internal) -> i64 {
    match date_sign(&d.date) {
        0 => d.time.signum() as i64,
        s => s,
    }
}

/// A time duration, checked: a RangeError past its limit.
pub(crate) fn checked(ns: i128) -> Result<i128> {
    iso::time_duration(ns).ok_or_else(|| range("time duration is out of range"))
}

/// RejectDuration: the fields (finite integers) of one sign, years,
/// months and weeks below 2³², the rest below 2⁵³ seconds in all.
pub(crate) fn reject_duration(d: &Dur) -> Result<Dur> {
    let mut sign = 0;
    for &v in d {
        let s = sign_of(v);
        if s != 0 {
            if sign != 0 && s != sign {
                return Err(range("mixed-sign values not allowed as duration fields"));
            }
            sign = s;
        }
    }
    if d[..3].iter().any(|v| v.abs() >= 4294967296.0) {
        return Err(range("years, months, and weeks must be < 2³²"));
    }
    let approx = d[3].abs() * 86400.0
        + d[4].abs() * 3600.0
        + d[5].abs() * 60.0
        + d[6].abs()
        + d[7].abs() / 1e3
        + d[8].abs() / 1e6
        + d[9].abs() / 1e9;
    if approx > 2f64.powi(54) {
        return Err(range("total of duration time units cannot exceed 9007199254740991.999999999 s"));
    }
    if time_ns(d).abs() > TIME_DURATION_MAX {
        return Err(range("total of duration time units cannot exceed 9007199254740991.999999999 s"));
    }
    let mut out = *d;
    for v in &mut out {
        *v += 0.0;
    }
    Ok(out)
}

/// The time fields (and days, as 24 hours) of a valid duration, in
/// nanoseconds.
fn time_ns(d: &Dur) -> i128 {
    let i = |k: usize| d[k] as i128;
    (((i(3) * 24 + i(4)) * 60 + i(5)) * 60 + i(6)) * 1_000_000_000 + i(7) * 1_000_000 + i(8) * 1000 + i(9)
}

/// ToInternalDurationRecord.
pub(crate) fn to_internal(d: &Dur) -> Internal {
    let mut t = *d;
    t[3] = 0.0;
    Internal { date: [d[0], d[1], d[2], d[3]], time: time_ns(&t) }
}

/// ToInternalDurationRecordWith24HourDays.
pub(crate) fn to_internal_24h(d: &Dur) -> Internal {
    Internal { date: [d[0], d[1], d[2], 0.0], time: time_ns(d) }
}

/// ToDateDurationRecordWithoutTime.
pub(crate) fn date_without_time(d: &Dur) -> Result<[f64; 4]> {
    let i = to_internal_24h(d);
    let days = (i.time / iso::NS_PER_DAY) as f64;
    reject_duration(&[i.date[0], i.date[1], i.date[2], days, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])?;
    Ok([i.date[0], i.date[1], i.date[2], days])
}

/// AdjustDateDurationRecord: `days` (and weeks, months) replaced, checked.
pub(crate) fn adjust(d: &[f64; 4], days: f64, weeks: Option<f64>, months: Option<f64>) -> Result<[f64; 4]> {
    let r = [d[0], months.unwrap_or(d[1]), weeks.unwrap_or(d[2]), days];
    reject_duration(&[r[0], r[1], r[2], r[3], 0.0, 0.0, 0.0, 0.0, 0.0, 0.0])?;
    Ok(r)
}

/// TemporalDurationFromInternal: the time balanced up to `largest`.
pub(crate) fn from_internal(i: &Internal, largest: Unit) -> Result<Dur> {
    let sign = i.time.signum();
    let a = i.time.abs();
    let (mut days, mut hours, mut minutes, mut seconds, mut ms, mut us) = (0i128, 0i128, 0i128, 0i128, 0i128, 0i128);
    let ns;
    match largest {
        Unit::Year | Unit::Month | Unit::Week | Unit::Day => {
            days = a / iso::NS_PER_DAY;
            let r = a % iso::NS_PER_DAY;
            hours = r / 3_600_000_000_000;
            minutes = r / 60_000_000_000 % 60;
            seconds = r / 1_000_000_000 % 60;
            ms = r / 1_000_000 % 1000;
            us = r / 1000 % 1000;
            ns = r % 1000;
        }
        Unit::Hour => {
            hours = a / 3_600_000_000_000;
            minutes = a / 60_000_000_000 % 60;
            seconds = a / 1_000_000_000 % 60;
            ms = a / 1_000_000 % 1000;
            us = a / 1000 % 1000;
            ns = a % 1000;
        }
        Unit::Minute => {
            minutes = a / 60_000_000_000;
            seconds = a / 1_000_000_000 % 60;
            ms = a / 1_000_000 % 1000;
            us = a / 1000 % 1000;
            ns = a % 1000;
        }
        Unit::Second => {
            seconds = a / 1_000_000_000;
            ms = a / 1_000_000 % 1000;
            us = a / 1000 % 1000;
            ns = a % 1000;
        }
        Unit::Millisecond => {
            ms = a / 1_000_000;
            us = a / 1000 % 1000;
            ns = a % 1000;
        }
        Unit::Microsecond => {
            us = a / 1000;
            ns = a % 1000;
        }
        Unit::Nanosecond => ns = a,
    }
    let f = |v: i128| (sign * v) as f64 + 0.0;
    reject_duration(&[
        i.date[0],
        i.date[1],
        i.date[2],
        i.date[3] + f(days),
        f(hours),
        f(minutes),
        f(seconds),
        f(ms),
        f(us),
        f(ns),
    ])
}

/// CombineDateAndTimeDuration.
fn combine(date: [f64; 4], time: i128) -> Internal {
    Internal { date, time }
}

/// DefaultTemporalLargestUnit.
pub(crate) fn default_largest_unit(d: &Dur) -> Unit {
    d.iter().position(|&v| v != 0.0).map_or(Unit::Nanosecond, |k| Unit::ALL[k])
}

pub(crate) fn negated(d: &Dur) -> Dur {
    d.map(|v| if v == 0.0 { 0.0 } else { -v })
}

/// A new Temporal.Duration.
pub(crate) fn create_duration(vm: &mut Vm, d: Dur) -> Value {
    create(vm, Slots::Duration(d))
}

/// ToTemporalPartialDurationRecord: the fields present (read in
/// alphabetical order), integers; a TypeError if none is.
fn partial_duration(vm: &mut Vm, item: Value) -> Result<[Option<f64>; 10]> {
    if !is_object(item) {
        return Err(type_err("invalid duration-like"));
    }
    // (days, hours, microseconds, milliseconds, minutes, months,
    // nanoseconds, seconds, weeks, years.)
    let order = [3usize, 4, 8, 7, 5, 1, 9, 6, 2, 0];
    let mut out = [None; 10];
    let mut any = false;
    for k in order {
        let v = get(vm, item, Unit::PLURAL[k])?;
        if !v.is_undefined() {
            out[k] = Some(to_integer_if_integral(vm, v)?);
            any = true;
        }
    }
    if !any {
        return Err(type_err("invalid duration-like: no duration fields"));
    }
    Ok(out)
}

/// ToTemporalDuration.
pub(crate) fn to_duration(vm: &mut Vm, item: Value) -> Result<Dur> {
    if let Some(Slots::Duration(d)) = slots(item) {
        return Ok(d);
    }
    if !is_object(item) {
        let s = require_string(item)?;
        let d = parse::parse_duration(&s).map_err(parse_err)?;
        return reject_duration(&d);
    }
    let p = partial_duration(vm, item)?;
    reject_duration(&p.map(|v| v.unwrap_or(0.0)))
}

/// TemporalDurationToString.
pub(crate) fn duration_string(d: &Dur, precision: iso::Precision) -> String {
    let sign = duration_sign(d);
    let num = |v: f64| {
        let a = v.abs();
        if a <= 9007199254740991.0 { format!("{a}") } else { format!("{}", a as u128) }
    };
    let mut date = String::new();
    for (k, c) in ["Y", "M", "W", "D"].iter().enumerate() {
        if d[k] != 0.0 {
            date += &format!("{}{c}", num(d[k]));
        }
    }
    let mut time = String::new();
    if d[4] != 0.0 {
        time += &format!("{}H", num(d[4]));
    }
    if d[5] != 0.0 {
        time += &format!("{}M", num(d[5]));
    }
    let secs = (d[6] as i128) * 1_000_000_000 + (d[7] as i128) * 1_000_000 + (d[8] as i128) * 1000 + d[9] as i128;
    let largest = default_largest_unit(d);
    if secs != 0 || largest >= Unit::Second || precision != iso::Precision::Auto {
        let a = secs.abs();
        let whole = a / 1_000_000_000;
        let sub = (a % 1_000_000_000) as i64;
        time += &format!("{whole}{}S", iso::fraction_string(sub, precision));
    }
    let mut out = format!("{}P{date}", if sign < 0 { "-" } else { "" });
    if !time.is_empty() {
        out += "T";
        out += &time;
    }
    out
}

// ---- Relative rounding ----

/// A starting point (`relativeTo`): none, a date, or an exact time in a
/// time zone.
#[derive(Clone, Copy, Debug)]
pub(crate) enum Relative {
    None,
    Plain(IsoDate),
    Zoned(i128, u32),
}

/// GetTemporalRelativeToOption.
pub(crate) fn relative_to(vm: &mut Vm, options: Value) -> Result<Relative> {
    use fields::Field;
    let v = get(vm, options, "relativeTo")?;
    if v.is_undefined() {
        return Ok(Relative::None);
    }
    let (date, time, z, behaviour, offset_ns, match_minutes);
    if is_object(v) {
        match slots(v) {
            Some(Slots::Zoned(ns, zz)) => return Ok(Relative::Zoned(ns, zz)),
            Some(Slots::Date(d)) => return Ok(Relative::Plain(d)),
            Some(Slots::DateTime(dt)) => return Ok(Relative::Plain(dt.date)),
            _ => {}
        }
        calendar_with_iso_default(vm, v)?;
        let which: Vec<Field> = fields::DATE_FIELDS
            .iter()
            .chain(fields::TIME_FIELDS)
            .copied()
            .chain([Field::Offset, Field::TimeZone])
            .collect();
        let f = fields::prepare(vm, v, &which, fields::Required::Some(&[]))?;
        date = fields::date_from_fields(&f, Overflow::Constrain)?;
        time = Some(fields::time_from_fields(&f, Overflow::Constrain)?);
        z = f.time_zone;
        behaviour = if f.offset.is_some() { OffsetBehaviour::Option } else { OffsetBehaviour::Wall };
        offset_ns = f.offset.as_deref().map_or(0, |s| parse::parse_offset(s).unwrap().ns);
        match_minutes = false;
    } else {
        let s = require_string(v)?;
        let r = parse::parse_date_time(&s).map_err(parse_err)?;
        let mut b = OffsetBehaviour::Option;
        let mut mm = false;
        z = match &r.tz {
            Some(t) => {
                let zz = time_zone_from_identifier(vm, t)?;
                if r.z {
                    b = OffsetBehaviour::Exact;
                } else if r.offset.is_none() {
                    b = OffsetBehaviour::Wall;
                }
                mm = !r.offset.is_some_and(|o| o.sub_minute);
                Some(zz)
            }
            None if r.z => return Err(range("Z designator not supported for PlainDate relativeTo")),
            None => None,
        };
        canonicalize_calendar(r.calendar.as_deref().unwrap_or("iso8601"))?;
        date = r.date();
        time = r.time;
        behaviour = b;
        offset_ns = r.offset.map_or(0, |o| o.ns);
        match_minutes = mm;
    }
    let Some(z) = z else {
        return Ok(Relative::Plain(check_date(date)?));
    };
    let offset_ns = if behaviour == OffsetBehaviour::Option { offset_ns } else { 0 };
    let ns = interpret_offset(
        vm,
        date,
        time,
        behaviour,
        offset_ns,
        z,
        Disambiguation::Compatible,
        OffsetOption::Reject,
        match_minutes,
    )?;
    Ok(Relative::Zoned(ns, z))
}

/// AddInstant.
pub(crate) fn add_instant(ns: i128, time: i128) -> Result<i128> {
    check_epoch_ns(ns + time)
}

/// AddZonedDateTime: the date part in calendar days of the zone's wall
/// time, the time part in exact time.
pub(crate) fn add_zoned(vm: &Vm, ns: i128, z: u32, d: &Internal, overflow: Overflow) -> Result<i128> {
    if date_sign(&d.date) == 0 {
        return add_instant(ns, d.time);
    }
    let dt = zone(vm, z).date_time_at(ns);
    let added = fields::date_add(dt.date, &d.date, overflow)?;
    let intermediate = epoch_ns_for(vm, z, DateTime { date: added, time: dt.time }, Disambiguation::Compatible)?;
    add_instant(intermediate, d.time)
}

/// DifferenceTime.
pub(crate) fn difference_time(a: Time, b: Time) -> i128 {
    b.nanos() as i128 - a.nanos() as i128
}

/// DifferenceISODateTime.
fn difference_date_time(a: DateTime, b: DateTime, largest: Unit) -> Result<Internal> {
    let mut time = difference_time(a.time, b.time);
    let time_sign = time.signum() as i64;
    let date_sign = a.date.cmp(&b.date) as i64;
    let mut adjusted = b.date;
    if date_sign == time_sign {
        adjusted = iso::add_days(adjusted, time_sign);
        time = checked(time - time_sign as i128 * iso::NS_PER_DAY)?;
    }
    let date_largest = Unit::Day.larger(largest);
    let mut diff = fields::date_until(a.date, adjusted, date_largest);
    if largest != date_largest {
        time = checked(time + diff[3] as i128 * iso::NS_PER_DAY)?;
        diff[3] = 0.0;
    }
    Ok(combine(diff, time))
}

/// DifferenceZonedDateTime.
fn difference_zoned(vm: &Vm, ns1: i128, ns2: i128, z: u32, largest: Unit) -> Result<Internal> {
    let diff = ns2 - ns1;
    if diff == 0 {
        return Ok(combine(ZERO_DATE, 0));
    }
    let sign: i64 = if diff < 0 { 1 } else { -1 };
    let start = zone(vm, z).date_time_at(ns1);
    let end = zone(vm, z).date_time_at(ns2);
    if start.date == end.date {
        return Ok(combine(ZERO_DATE, diff));
    }
    let mut correction = 0;
    let max_correction = if sign == -1 { 2 } else { 1 };
    let mut time = difference_time(start.time, end.time);
    if time.signum() as i64 == sign {
        correction += 1;
    }
    let mut intermediate = start;
    while correction <= max_correction {
        let date = iso::add_days(end.date, correction * sign);
        intermediate = DateTime { date, time: start.time };
        let ins = epoch_ns_for(vm, z, intermediate, Disambiguation::Compatible)?;
        time = ns2 - ins;
        if time.signum() as i64 != sign {
            break;
        }
        correction += 1;
    }
    if correction > max_correction {
        return Err(range("time zone returned inconsistent instants"));
    }
    let date_largest = Unit::Day.larger(largest);
    let d = fields::date_until(start.date, intermediate.date, date_largest);
    Ok(combine(d, checked(time)?))
}

/// The epoch nanoseconds of `dt` in zone `z` (compatibly), or as UTC if
/// there is none.
fn epoch_of(vm: &Vm, z: Option<u32>, dt: DateTime) -> Result<i128> {
    match z {
        Some(z) => epoch_ns_for(vm, z, dt, Disambiguation::Compatible),
        None => Ok(iso::utc_epoch_ns(dt)),
    }
}

/// A nudge's result: the duration, the exact time it ends at, and
/// whether a calendar unit expanded.
struct Nudge {
    duration: Internal,
    nudged: i128,
    expanded: bool,
}

/// ComputeNudgeWindow.
#[allow(clippy::too_many_arguments)]
fn nudge_window(
    vm: &Vm,
    sign: i64,
    d: &Internal,
    origin: i128,
    dt: DateTime,
    z: Option<u32>,
    increment: i64,
    unit: Unit,
    shift: bool,
) -> Result<(f64, f64, i128, i128, [f64; 4], [f64; 4])> {
    let inc = increment as f64;
    let s = sign as f64;
    let trunc = |v: f64| iso::round_to_increment(v as i128, increment as i128, RoundingMode::Trunc) as f64;
    let (r1, r2, start, end) = match unit {
        Unit::Year => {
            let years = trunc(d.date[0]);
            let r1 = if shift { years + inc * s } else { years };
            let r2 = r1 + inc * s;
            (r1, r2, [r1, 0.0, 0.0, 0.0], [r2, 0.0, 0.0, 0.0])
        }
        Unit::Month => {
            let months = trunc(d.date[1]);
            let r1 = if shift { months + inc * s } else { months };
            let r2 = r1 + inc * s;
            (r1, r2, adjust(&d.date, 0.0, Some(0.0), Some(r1))?, adjust(&d.date, 0.0, Some(0.0), Some(r2))?)
        }
        Unit::Week => {
            let years_months = adjust(&d.date, 0.0, Some(0.0), None)?;
            let weeks_start = fields::date_add(dt.date, &years_months, Overflow::Constrain)?;
            let weeks_end = iso::add_days(weeks_start, d.date[3] as i64);
            let until = fields::date_until(weeks_start, weeks_end, Unit::Week);
            let weeks = trunc(d.date[2] + until[2]);
            let r2 = weeks + inc * s;
            (weeks, r2, adjust(&d.date, 0.0, Some(weeks), None)?, adjust(&d.date, 0.0, Some(r2), None)?)
        }
        _ => {
            let days = trunc(d.date[3]);
            let r2 = days + inc * s;
            (days, r2, adjust(&d.date, days, None, None)?, adjust(&d.date, r2, None, None)?)
        }
    };
    let start_ns = if date_sign(&start) == 0 {
        origin
    } else {
        let date = fields::date_add(dt.date, &start, Overflow::Constrain)?;
        epoch_of(vm, z, DateTime { date, time: dt.time })?
    };
    let date = fields::date_add(dt.date, &end, Overflow::Constrain)?;
    let end_ns = epoch_of(vm, z, DateTime { date, time: dt.time })?;
    Ok((r1, r2, start_ns, end_ns, start, end))
}

/// NudgeToCalendarUnit: the nudge, and the total (with an increment of 1).
#[allow(clippy::too_many_arguments)]
fn nudge_to_calendar_unit(
    vm: &Vm,
    sign: i64,
    d: &Internal,
    origin: i128,
    dest: i128,
    dt: DateTime,
    z: Option<u32>,
    increment: i64,
    unit: Unit,
    mode: RoundingMode,
) -> Result<(Nudge, Option<f64>)> {
    let mut expanded = false;
    let mut w = nudge_window(vm, sign, d, origin, dt, z, increment, unit, false)?;
    let inside = |w: &(f64, f64, i128, i128, [f64; 4], [f64; 4])| {
        if sign == 1 { w.2 <= dest && dest <= w.3 } else { w.3 <= dest && dest <= w.2 }
    };
    if !inside(&w) {
        w = nudge_window(vm, sign, d, origin, dt, z, increment, unit, true)?;
        if !inside(&w) {
            return Err(range(format!("{} was 0 days long", unit.name())));
        }
        expanded = true;
    }
    let (r1, r2, start_ns, end_ns, start, end) = w;
    if start_ns == end_ns {
        return Err(range("start and end of a rounding window are equal"));
    }
    let numerator = dest - start_ns;
    let denominator = end_ns - start_ns;
    let even = ((r1.abs() / increment as f64) as i64) % 2 == 0;
    let rounded = if numerator == 0 {
        r1.abs()
    } else if numerator == denominator {
        r2.abs()
    } else if iso::round_between(numerator, denominator, even, sign, mode) {
        r2.abs()
    } else {
        r1.abs()
    };
    let total = (increment == 1).then(|| {
        let fake = denominator * r1 as i128 + numerator * sign as i128;
        iso::fdiv(fake, denominator)
    });
    let up = rounded == r2.abs();
    expanded = expanded || up;
    let duration = combine(if up { end } else { start }, 0);
    Ok((Nudge { duration, nudged: if expanded { end_ns } else { start_ns }, expanded }, total))
}

/// NudgeToZonedTime.
#[allow(clippy::too_many_arguments)]
fn nudge_to_zoned_time(vm: &Vm, sign: i64, d: &Internal, dt: DateTime, z: u32, increment: i64, unit: Unit, mode: RoundingMode) -> Result<Nudge> {
    let start = fields::date_add(dt.date, &d.date, Overflow::Constrain)?;
    let end = iso::add_days(start, sign);
    let start_ns = epoch_ns_for(vm, z, DateTime { date: start, time: dt.time }, Disambiguation::Compatible)?;
    let end_ns = epoch_ns_for(vm, z, DateTime { date: end, time: dt.time }, Disambiguation::Compatible)?;
    let day_span = end_ns - start_ns;
    if day_span.signum() as i64 != sign {
        return Err(range("time zone returned inconsistent instants"));
    }
    let unit_increment = unit.nanos() * increment as i128;
    let mut rounded = checked(iso::round_to_increment(d.time, unit_increment, mode))?;
    let beyond = checked(rounded - day_span)?;
    let past = beyond.signum() as i64 != -sign;
    let (delta, nudged) = if past {
        rounded = checked(iso::round_to_increment(beyond, unit_increment, mode))?;
        (sign, rounded + end_ns)
    } else {
        (0, rounded + start_ns)
    };
    let date = adjust(&d.date, d.date[3] + delta as f64, None, None)?;
    Ok(Nudge { duration: combine(date, rounded), nudged, expanded: past })
}

/// NudgeToDayOrTime.
fn nudge_to_day_or_time(d: &Internal, dest: i128, largest: Unit, increment: i64, smallest: Unit, mode: RoundingMode) -> Result<Nudge> {
    let time = checked(d.time + d.date[3] as i128 * iso::NS_PER_DAY)?;
    let rounded = checked(iso::round_to_increment(time, smallest.nanos() * increment as i128, mode))?;
    let diff = rounded - time;
    let whole = time / iso::NS_PER_DAY;
    let rounded_whole = rounded / iso::NS_PER_DAY;
    let expanded = (rounded_whole - whole).signum() == time.signum();
    let nudged = dest + diff;
    let (days, remainder) = if largest.is_date() {
        (rounded_whole as f64, rounded - rounded_whole * iso::NS_PER_DAY)
    } else {
        (0.0, rounded)
    };
    let date = adjust(&d.date, days, None, None)?;
    Ok(Nudge { duration: combine(date, remainder), nudged, expanded })
}

/// BubbleRelativeDuration: units below `largest` that reached a whole
/// larger unit carried into it.
#[allow(clippy::too_many_arguments)]
fn bubble(vm: &Vm, sign: i64, d: Internal, nudged: i128, dt: DateTime, z: Option<u32>, largest: Unit, smallest: Unit) -> Result<Internal> {
    if smallest == largest {
        return Ok(d);
    }
    let mut d = d;
    let mut k = smallest as usize;
    while k > largest as usize {
        k -= 1;
        let unit = Unit::ALL[k];
        if unit == Unit::Week && largest != Unit::Week {
            continue;
        }
        let s = sign as f64;
        let end = match unit {
            Unit::Year => [d.date[0] + s, 0.0, 0.0, 0.0],
            Unit::Month => adjust(&d.date, 0.0, Some(0.0), Some(d.date[1] + s))?,
            _ => adjust(&d.date, 0.0, Some(d.date[2] + s), None)?,
        };
        let date = fields::date_add(dt.date, &end, Overflow::Constrain)?;
        let end_ns = epoch_of(vm, z, DateTime { date, time: dt.time })?;
        let cmp = nudged.cmp(&end_ns) as i64;
        if cmp != -sign {
            d = combine(end, 0);
        } else {
            break;
        }
    }
    Ok(d)
}

/// RoundRelativeDuration.
#[allow(clippy::too_many_arguments)]
fn round_relative(
    vm: &Vm,
    d: Internal,
    origin: i128,
    dest: i128,
    dt: DateTime,
    z: Option<u32>,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> Result<Internal> {
    let irregular = smallest.is_calendar() || (z.is_some() && smallest == Unit::Day);
    let sign = if internal_sign(&d) < 0 { -1 } else { 1 };
    let nudge = if irregular {
        nudge_to_calendar_unit(vm, sign, &d, origin, dest, dt, z, increment, smallest, mode)?.0
    } else if let Some(z) = z {
        nudge_to_zoned_time(vm, sign, &d, dt, z, increment, smallest, mode)?
    } else {
        nudge_to_day_or_time(&d, dest, largest, increment, smallest, mode)?
    };
    let mut out = nudge.duration;
    if nudge.expanded && smallest != Unit::Week {
        out = bubble(vm, sign, out, nudge.nudged, dt, z, largest, smallest.larger(Unit::Day))?;
    }
    Ok(out)
}

/// TotalRelativeDuration.
fn total_relative(vm: &Vm, d: Internal, origin: i128, dest: i128, dt: DateTime, z: Option<u32>, unit: Unit) -> Result<f64> {
    if unit.is_calendar() || (z.is_some() && unit == Unit::Day) {
        let sign = if internal_sign(&d) < 0 { -1 } else { 1 };
        let (_, total) = nudge_to_calendar_unit(vm, sign, &d, origin, dest, dt, z, 1, unit, RoundingMode::Trunc)?;
        return Ok(total.unwrap());
    }
    let time = checked(d.time + d.date[3] as i128 * iso::NS_PER_DAY)?;
    Ok(iso::fdiv(time, unit.nanos()))
}

/// DifferencePlainDateTimeWithRounding.
pub(crate) fn difference_plain_rounded(
    vm: &Vm,
    a: DateTime,
    b: DateTime,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> Result<Internal> {
    if a == b {
        return Ok(combine(ZERO_DATE, 0));
    }
    check_date_time(a)?;
    check_date_time(b)?;
    let d = difference_date_time(a, b, largest)?;
    if smallest == Unit::Nanosecond && increment == 1 {
        return Ok(d);
    }
    let (origin, dest) = (iso::utc_epoch_ns(a), iso::utc_epoch_ns(b));
    round_relative(vm, d, origin, dest, a, None, largest, increment, smallest, mode)
}

/// RoundRelativeDuration from `start` to `end` (plain date-times, no time
/// zone): what PlainYearMonth's differences round with.
#[allow(clippy::too_many_arguments)]
pub(crate) fn round_relative_plain(
    vm: &Vm,
    d: Internal,
    start: DateTime,
    end: DateTime,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> Result<Internal> {
    round_relative(vm, d, iso::utc_epoch_ns(start), iso::utc_epoch_ns(end), start, None, largest, increment, smallest, mode)
}

/// DifferencePlainDateTimeWithTotal.
pub(crate) fn difference_plain_total(vm: &Vm, a: DateTime, b: DateTime, unit: Unit) -> Result<f64> {
    if a == b {
        return Ok(0.0);
    }
    check_date_time(a)?;
    check_date_time(b)?;
    let d = difference_date_time(a, b, unit)?;
    if unit == Unit::Nanosecond {
        return Ok(d.time as f64);
    }
    total_relative(vm, d, iso::utc_epoch_ns(a), iso::utc_epoch_ns(b), a, None, unit)
}

/// DifferenceInstant.
pub(crate) fn difference_instant(ns1: i128, ns2: i128, increment: i64, smallest: Unit, mode: RoundingMode) -> Result<Internal> {
    let t = round_time_duration(ns2 - ns1, increment, smallest, mode)?;
    Ok(combine(ZERO_DATE, t))
}

/// RoundTimeDuration.
pub(crate) fn round_time_duration(t: i128, increment: i64, unit: Unit, mode: RoundingMode) -> Result<i128> {
    checked(iso::round_to_increment(t, unit.nanos() * increment as i128, mode))
}

/// DifferenceZonedDateTimeWithRounding.
#[allow(clippy::too_many_arguments)]
pub(crate) fn difference_zoned_rounded(
    vm: &Vm,
    ns1: i128,
    ns2: i128,
    z: u32,
    largest: Unit,
    increment: i64,
    smallest: Unit,
    mode: RoundingMode,
) -> Result<Internal> {
    if !largest.is_date() {
        return difference_instant(ns1, ns2, increment, smallest, mode);
    }
    let d = difference_zoned(vm, ns1, ns2, z, largest)?;
    if smallest == Unit::Nanosecond && increment == 1 {
        return Ok(d);
    }
    let dt = zone(vm, z).date_time_at(ns1);
    round_relative(vm, d, ns1, ns2, dt, Some(z), largest, increment, smallest, mode)
}

/// DifferenceZonedDateTimeWithTotal.
fn difference_zoned_total(vm: &Vm, ns1: i128, ns2: i128, z: u32, unit: Unit) -> Result<f64> {
    if !unit.is_date() {
        return Ok(iso::fdiv(ns2 - ns1, unit.nanos()));
    }
    let d = difference_zoned(vm, ns1, ns2, z, unit)?;
    let dt = zone(vm, z).date_time_at(ns1);
    total_relative(vm, d, ns1, ns2, dt, Some(z), unit)
}

/// The settings of a difference (`until`, `since`): GetDifferenceSettings.
pub(crate) struct Settings {
    pub largest: Unit,
    pub smallest: Unit,
    pub increment: i64,
    pub mode: RoundingMode,
}

/// GetDifferenceSettings (`since`: the rounding mode negated).
pub(crate) fn difference_settings(
    vm: &mut Vm,
    since: bool,
    options: Value,
    group: Group,
    disallowed: &[Unit],
    fallback_smallest: Unit,
    default_largest: Unit,
) -> Result<Settings> {
    let largest = unit_option(vm, options, "largestUnit", false)?;
    let increment = rounding_increment(vm, options)?;
    let mut mode = rounding_mode(vm, options, RoundingMode::Trunc)?;
    let smallest = unit_option(vm, options, "smallestUnit", false)?;
    validate_unit(largest, group, true)?;
    if let UnitOption::Unit(u) = largest {
        if disallowed.contains(&u) {
            return Err(range(format!("largestUnit {} is not allowed", u.name())));
        }
    }
    if since {
        mode = mode.negate();
    }
    validate_unit(smallest, group, false)?;
    let smallest = match smallest {
        UnitOption::Unit(u) => u,
        _ => fallback_smallest,
    };
    if disallowed.contains(&smallest) {
        return Err(range(format!("smallestUnit {} is not allowed", smallest.name())));
    }
    let default = default_largest.larger(smallest);
    let largest = match largest {
        UnitOption::Unit(u) => u,
        _ => default,
    };
    if largest.larger(smallest) != largest {
        return Err(range(format!("largestUnit {} cannot be smaller than smallestUnit {}", largest.name(), smallest.name())));
    }
    if let Some(max) = max_increment(smallest) {
        validate_increment(increment, max, false)?;
    }
    Ok(Settings { largest, smallest, increment, mode })
}

/// The maximum rounding increment of a time unit (none for a date unit).
pub(crate) fn max_increment(u: Unit) -> Option<i64> {
    match u {
        Unit::Hour => Some(24),
        Unit::Minute | Unit::Second => Some(60),
        Unit::Millisecond | Unit::Microsecond | Unit::Nanosecond => Some(1000),
        _ => None,
    }
}

/// DateDurationDays: a date duration's days, its years, months and weeks
/// counted from `relative`.
fn date_duration_days(d: &[f64; 4], relative: IsoDate) -> Result<f64> {
    let ymw = adjust(d, 0.0, None, None)?;
    if date_sign(&ymw) == 0 {
        return Ok(d[3]);
    }
    let later = fields::date_add(relative, &ymw, Overflow::Constrain)?;
    Ok(d[3] + (iso::date_epoch_days(later) - iso::date_epoch_days(relative)) as f64)
}

// ---- Temporal.Duration ----

fn this_duration(this: Value) -> Result<Dur> {
    match this_slots(this, Kind::Duration)? {
        Slots::Duration(d) => Ok(d),
        _ => unreachable!(),
    }
}

fn duration_ctor(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        return Err(type_err("Constructor Temporal.Duration requires 'new'"));
    }
    let mut d = [0f64; 10];
    for (k, v) in d.iter_mut().enumerate() {
        let x = arg(a, k);
        if !x.is_undefined() {
            *v = to_integer_if_integral(vm, x)?;
        }
    }
    let d = reject_duration(&d)?;
    Ok(create_duration(vm, d))
}

fn field<const K: usize>(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::number(this_duration(this)?[K]))
}

fn duration_sign_getter(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_i32(duration_sign(&this_duration(this)?) as i32))
}

fn duration_blank(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(Value::from_bool(duration_sign(&this_duration(this)?) == 0))
}

fn duration_with(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    let p = partial_duration(vm, arg(a, 0))?;
    let r = reject_duration(&std::array::from_fn(|k| p[k].unwrap_or(d[k])))?;
    Ok(create_duration(vm, r))
}

fn duration_negated(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    Ok(create_duration(vm, negated(&d)))
}

fn duration_abs(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    Ok(create_duration(vm, d.map(f64::abs)))
}

/// AddDurations.
fn add_durations(vm: &mut Vm, this: Value, a: &[Value], subtract: bool) -> Result<Value> {
    let d = this_duration(this)?;
    let mut other = to_duration(vm, arg(a, 0))?;
    if subtract {
        other = negated(&other);
    }
    let largest = default_largest_unit(&d).larger(default_largest_unit(&other));
    if largest.is_calendar() {
        return Err(range("for years, months, or weeks arithmetic, use date arithmetic relative to a starting point"));
    }
    let t = checked(to_internal_24h(&d).time + to_internal_24h(&other).time)?;
    let r = from_internal(&combine(ZERO_DATE, t), largest)?;
    Ok(create_duration(vm, r))
}

fn duration_add(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    add_durations(vm, this, a, false)
}

fn duration_subtract(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    add_durations(vm, this, a, true)
}

/// An options argument that may be a string (the value of option `name`).
fn options_or_string(vm: &mut Vm, v: Value, name: &str) -> Result<Value> {
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

fn duration_round(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    let existing = default_largest_unit(&d);
    let o = options_or_string(vm, arg(a, 0), "smallestUnit")?;
    let largest_opt = unit_option(vm, o, "largestUnit", false)?;
    let relative = relative_to(vm, o)?;
    let increment = rounding_increment(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::HalfExpand)?;
    let smallest_opt = unit_option(vm, o, "smallestUnit", false)?;
    validate_unit(smallest_opt, Group::DateTime, false)?;
    let smallest = match smallest_opt {
        UnitOption::Unit(u) => u,
        _ => Unit::Nanosecond,
    };
    let default = existing.larger(smallest);
    let mut largest = match largest_opt {
        UnitOption::Unit(u) => u,
        _ => default,
    };
    if smallest_opt == UnitOption::Unset && largest_opt == UnitOption::Unset {
        return Err(range("at least one of smallestUnit or largestUnit is required"));
    }
    if largest.larger(smallest) != largest {
        return Err(range(format!("largestUnit {} cannot be smaller than smallestUnit {}", largest.name(), smallest.name())));
    }
    if let Some(max) = max_increment(smallest) {
        validate_increment(increment, max, false)?;
    }
    if increment > 1 && smallest.is_date() && largest != smallest {
        return Err(range("for calendar units with roundingIncrement > 1, use largestUnit = smallestUnit"));
    }
    let r = match relative {
        Relative::Zoned(ns, z) => {
            let i = to_internal(&d);
            let target = add_zoned(vm, ns, z, &i, Overflow::Constrain)?;
            let diff = difference_zoned_rounded(vm, ns, target, z, largest, increment, smallest, mode)?;
            if largest.is_date() {
                largest = Unit::Hour;
            }
            from_internal(&diff, largest)?
        }
        Relative::Plain(date) => {
            let i = to_internal_24h(&d);
            let (days, time) = iso::balance_time(i.time);
            let dd = adjust(&i.date, days as f64, None, None)?;
            let target = fields::date_add(date, &dd, Overflow::Constrain)?;
            let start = DateTime { date, time: Time::MIDNIGHT };
            let diff = difference_plain_rounded(vm, start, DateTime { date: target, time }, largest, increment, smallest, mode)?;
            from_internal(&diff, largest)?
        }
        Relative::None => {
            if existing.is_calendar() || largest.is_calendar() {
                return Err(range("a starting point is required for years, months, or weeks balancing"));
            }
            let i = to_internal_24h(&d);
            let t = round_time_duration(i.time, increment, smallest, mode)?;
            let internal = if smallest == Unit::Day {
                combine([0.0, 0.0, 0.0, (t / iso::NS_PER_DAY) as f64], 0)
            } else {
                combine(ZERO_DATE, t)
            };
            from_internal(&internal, largest)?
        }
    };
    Ok(create_duration(vm, r))
}

fn duration_total(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    let o = options_or_string(vm, arg(a, 0), "unit")?;
    let relative = relative_to(vm, o)?;
    let unit = match unit_option(vm, o, "unit", true)? {
        UnitOption::Unit(u) => u,
        _ => return Err(range("unit must be a unit")),
    };
    let total = match relative {
        Relative::Zoned(ns, z) => {
            let target = add_zoned(vm, ns, z, &to_internal(&d), Overflow::Constrain)?;
            difference_zoned_total(vm, ns, target, z, unit)?
        }
        Relative::Plain(date) => {
            let i = to_internal_24h(&d);
            let (days, time) = iso::balance_time(i.time);
            let dd = adjust(&i.date, days as f64, None, None)?;
            let target = fields::date_add(date, &dd, Overflow::Constrain)?;
            difference_plain_total(vm, DateTime { date, time: Time::MIDNIGHT }, DateTime { date: target, time }, unit)?
        }
        Relative::None => {
            let largest = default_largest_unit(&d);
            if largest.is_calendar() || unit.is_calendar() {
                return Err(range("a starting point is required for years, months, or weeks totals"));
            }
            iso::fdiv(to_internal_24h(&d).time, unit.nanos())
        }
    };
    Ok(Value::number(total))
}

fn duration_to_string(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    let o = options_object(vm, arg(a, 0))?;
    let digits = fractional_digits(vm, o)?;
    let mode = rounding_mode(vm, o, RoundingMode::Trunc)?;
    let smallest = unit_option(vm, o, "smallestUnit", false)?;
    validate_unit(smallest, Group::Time, false)?;
    if matches!(smallest, UnitOption::Unit(Unit::Hour | Unit::Minute)) {
        return Err(range("smallestUnit must be a time unit other than hours or minutes"));
    }
    let (precision, unit, increment) = seconds_precision(smallest, digits);
    if unit == Unit::Nanosecond && increment == 1 {
        return Ok(string(vm, &duration_string(&d, precision)));
    }
    let largest = default_largest_unit(&d);
    let i = to_internal(&d);
    let t = round_time_duration(i.time, increment, unit, mode)?;
    let r = from_internal(&combine(i.date, t), largest.larger(Unit::Second))?;
    Ok(string(vm, &duration_string(&r, precision)))
}

fn duration_to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let d = this_duration(this)?;
    Ok(string(vm, &duration_string(&d, iso::Precision::Auto)))
}

fn duration_from(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let d = to_duration(vm, arg(a, 0))?;
    Ok(create_duration(vm, d))
}

fn duration_compare(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let one = to_duration(vm, arg(a, 0))?;
    let two = to_duration(vm, arg(a, 1))?;
    let o = options_object(vm, arg(a, 2))?;
    let relative = relative_to(vm, o)?;
    if one == two {
        return Ok(Value::from_i32(0));
    }
    let (l1, l2) = (default_largest_unit(&one), default_largest_unit(&two));
    let (d1, d2) = (to_internal(&one), to_internal(&two));
    if let Relative::Zoned(ns, z) = relative {
        if l1.is_date() || l2.is_date() {
            let a1 = add_zoned(vm, ns, z, &d1, Overflow::Constrain)?;
            let a2 = add_zoned(vm, ns, z, &d2, Overflow::Constrain)?;
            return Ok(Value::from_i32(a1.cmp(&a2) as i32));
        }
    }
    let (mut days1, mut days2) = (d1.date[3], d2.date[3]);
    if l1.is_calendar() || l2.is_calendar() {
        let Relative::Plain(date) = relative else {
            return Err(range("a starting point is required for years, months, or weeks comparison"));
        };
        days1 = date_duration_days(&d1.date, date)?;
        days2 = date_duration_days(&d2.date, date)?;
    }
    let t1 = checked(d1.time + days1 as i128 * iso::NS_PER_DAY)?;
    let t2 = checked(d2.time + days2 as i128 * iso::NS_PER_DAY)?;
    Ok(Value::from_i32(t1.cmp(&t2) as i32))
}

pub(crate) fn spec() -> ClassSpec {
    ClassSpec {
        name: "Duration",
        kind: Kind::Duration,
        ctor: duration_ctor,
        length: 0,
        statics: &[("from", duration_from as NativeFn, 1), ("compare", duration_compare, 2)],
        getters: &[
            ("years", field::<0>),
            ("months", field::<1>),
            ("weeks", field::<2>),
            ("days", field::<3>),
            ("hours", field::<4>),
            ("minutes", field::<5>),
            ("seconds", field::<6>),
            ("milliseconds", field::<7>),
            ("microseconds", field::<8>),
            ("nanoseconds", field::<9>),
            ("sign", duration_sign_getter),
            ("blank", duration_blank),
        ],
        methods: &[
            ("with", duration_with as NativeFn, 1),
            ("negated", duration_negated, 0),
            ("abs", duration_abs, 0),
            ("add", duration_add, 1),
            ("subtract", duration_subtract, 1),
            ("round", duration_round, 1),
            ("total", duration_total, 1),
            ("toString", duration_to_string, 0),
            ("toJSON", duration_to_json, 0),
            ("toLocaleString", duration_to_json, 0),
        ],
    }
}
