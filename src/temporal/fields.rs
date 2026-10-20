//! Property bags (PrepareCalendarFields and its kin), the ISO calendar's
//! operations on fields, and the conversions of values to Temporal
//! records (ToTemporalDate and its kin).

use super::iso::{self, DateTime, IsoDate, Time};
use super::*;

/// A property bag's fields, in alphabetical order (the order they are
/// read in).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Field {
    Day,
    Hour,
    Microsecond,
    Millisecond,
    Minute,
    Month,
    MonthCode,
    Nanosecond,
    Offset,
    Second,
    TimeZone,
    Year,
}

const FIELD_NAMES: [&str; 12] =
    ["day", "hour", "microsecond", "millisecond", "minute", "month", "monthCode", "nanosecond", "offset", "second", "timeZone", "year"];

pub(crate) const DATE_FIELDS: &[Field] = &[Field::Year, Field::Month, Field::MonthCode, Field::Day];
pub(crate) const TIME_FIELDS: &[Field] =
    &[Field::Hour, Field::Minute, Field::Second, Field::Millisecond, Field::Microsecond, Field::Nanosecond];

/// A bag's fields, converted: the date's, the time's (hour, minute,
/// second, millisecond, microsecond, nanosecond), an offset (a valid
/// offset string) and a time zone.
#[derive(Clone, Default, Debug)]
pub(crate) struct Fields {
    pub year: Option<f64>,
    pub month: Option<f64>,
    /// A month code's number, and whether it's a leap month's (`M05L`).
    pub month_code: Option<(u8, bool)>,
    pub day: Option<f64>,
    pub time: [Option<f64>; 6],
    pub offset: Option<String>,
    pub time_zone: Option<u32>,
}

/// Which fields must be present: those listed (a TypeError if one is
/// missing), or at least one of any (`Partial`).
pub(crate) enum Required<'a> {
    Some(&'a [Field]),
    Partial,
}

/// ToMonthCode: `M01` to `M99`, or `M00L` to `M99L` (as syntax only).
fn to_month_code(vm: &mut Vm, v: Value) -> Result<(u8, bool)> {
    let p = to_primitive_string(vm, v)?;
    let s = require_string(p).map_err(|_| type_err("month code must be a string"))?;
    let b = s.as_bytes();
    let ok = (b.len() == 3 || b.len() == 4 && b[3] == b'L') && b[0] == b'M' && b[1].is_ascii_digit() && b[2].is_ascii_digit();
    let n = if ok { (b[1] - b'0') * 10 + (b[2] - b'0') } else { 0 };
    if !ok || (n == 0 && b.len() == 3) {
        return Err(range(format!("bad month code {s}; must match M01-M99 or M00L-M99L")));
    }
    Ok((n, b.len() == 4))
}

/// PrepareCalendarFields: the bag's fields `which` read, in alphabetical
/// order, each converted as it is read.
pub(crate) fn prepare(vm: &mut Vm, bag: Value, which: &[Field], required: Required) -> Result<Fields> {
    let mut sorted: Vec<Field> = which.to_vec();
    sorted.sort_by_key(|&f| f as u8);
    let mut out = Fields::default();
    let mut any = false;
    for f in sorted {
        let v = get(vm, bag, FIELD_NAMES[f as usize])?;
        if v.is_undefined() {
            match &required {
                Required::Partial => {}
                Required::Some(req) => {
                    if req.contains(&f) {
                        return Err(type_err(format!("required property '{}' missing or undefined", FIELD_NAMES[f as usize])));
                    }
                    if let Some(k) = time_index(f) {
                        out.time[k] = Some(0.0);
                    }
                }
            }
            continue;
        }
        any = true;
        match f {
            Field::Year => out.year = Some(to_integer(vm, v)?),
            Field::Month => out.month = Some(to_positive_integer(vm, v, "month")?),
            Field::MonthCode => out.month_code = Some(to_month_code(vm, v)?),
            Field::Day => out.day = Some(to_positive_integer(vm, v, "day")?),
            Field::Offset => {
                let p = to_primitive_string(vm, v)?;
                let s = require_string(p)?;
                parse::parse_offset(&s).map_err(parse_err)?;
                out.offset = Some(s);
            }
            Field::TimeZone => out.time_zone = Some(to_time_zone(vm, v)?),
            _ => out.time[time_index(f).unwrap()] = Some(to_integer(vm, v)?),
        }
    }
    if matches!(required, Required::Partial) && !any {
        return Err(type_err("no supported properties found"));
    }
    Ok(out)
}

fn time_index(f: Field) -> Option<usize> {
    Some(match f {
        Field::Hour => 0,
        Field::Minute => 1,
        Field::Second => 2,
        Field::Millisecond => 3,
        Field::Microsecond => 4,
        Field::Nanosecond => 5,
        _ => return None,
    })
}

/// ToTemporalTimeRecord (`partial`: absent fields stay absent): a TypeError
/// if none is present.
pub(crate) fn time_record(vm: &mut Vm, bag: Value, partial: bool) -> Result<[Option<f64>; 6]> {
    // (hour, microsecond, millisecond, minute, nanosecond, second.)
    let order = [0usize, 4, 3, 1, 5, 2];
    let mut out = [None; 6];
    let mut any = false;
    for k in order {
        let v = get(vm, bag, ["hour", "minute", "second", "millisecond", "microsecond", "nanosecond"][k])?;
        if !v.is_undefined() {
            out[k] = Some(to_integer(vm, v)?);
            any = true;
        } else if !partial {
            out[k] = Some(0.0);
        }
    }
    if !any {
        return Err(type_err("invalid time-like: no time fields"));
    }
    Ok(out)
}

/// ISODateToFields: a date's month code, and its year and day as `type`
/// has them.
pub(crate) fn date_fields(d: IsoDate, year: bool, day: bool) -> Fields {
    Fields {
        year: year.then_some(d.year as f64),
        month_code: Some((d.month, false)),
        day: day.then_some(d.day as f64),
        ..Fields::default()
    }
}

/// CalendarMergeFields: `additional`'s fields over `fields`' (a month
/// replacing a month code and vice versa).
pub(crate) fn merge(fields: &Fields, additional: &Fields) -> Fields {
    let month_given = additional.month.is_some() || additional.month_code.is_some();
    Fields {
        year: additional.year.or(fields.year),
        month: if month_given { additional.month } else { fields.month },
        month_code: if month_given { additional.month_code } else { fields.month_code },
        day: additional.day.or(fields.day),
        time: std::array::from_fn(|k| additional.time[k].or(fields.time[k])),
        offset: additional.offset.clone().or_else(|| fields.offset.clone()),
        time_zone: additional.time_zone.or(fields.time_zone),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum FieldsType {
    Date,
    YearMonth,
    MonthDay,
}

/// The ISO calendar's ResolveFields: the year and day required as `type`
/// needs them (TypeErrors), the month from the month or month code
/// (which must agree; an ISO month code is `M01`-`M12`).
fn resolve(f: &Fields, t: FieldsType) -> Result<f64> {
    if t != FieldsType::MonthDay && f.year.is_none() {
        return Err(type_err("year is required"));
    }
    if t != FieldsType::YearMonth && f.day.is_none() {
        return Err(type_err("day is required"));
    }
    match f.month_code {
        None => f.month.ok_or_else(|| type_err("either month or monthCode is required")),
        Some((n, leap)) => {
            if leap || !(1..=12).contains(&n) {
                return Err(range(format!("invalid monthCode for the ISO calendar: M{n:02}{}", if leap { "L" } else { "" })));
            }
            if f.month.is_some_and(|m| m != n as f64) {
                return Err(range("month and monthCode must match"));
            }
            Ok(n as f64)
        }
    }
}

/// CalendarDateFromFields.
pub(crate) fn date_from_fields(f: &Fields, overflow: Overflow) -> Result<IsoDate> {
    let month = resolve(f, FieldsType::Date)?;
    let d = regulate_iso_date(f.year.unwrap(), month, f.day.unwrap(), overflow)?;
    check_date(d)
}

/// CalendarYearMonthFromFields.
pub(crate) fn year_month_from_fields(f: &Fields, overflow: Overflow) -> Result<IsoDate> {
    let month = resolve(f, FieldsType::YearMonth)?;
    let d = regulate_iso_date(f.year.unwrap(), month, 1.0, overflow)?;
    check_year_month(d)
}

/// CalendarMonthDayFromFields: the reference year 1972 (a leap year).
pub(crate) fn month_day_from_fields(f: &Fields, overflow: Overflow) -> Result<IsoDate> {
    let month = resolve(f, FieldsType::MonthDay)?;
    let d = regulate_iso_date(f.year.unwrap_or(1972.0), month, f.day.unwrap(), overflow)?;
    check_date(IsoDate { year: 1972, ..d })
}

/// The time of `f`'s time fields (absent ones 0), regulated.
pub(crate) fn time_from_fields(f: &Fields, overflow: Overflow) -> Result<Time> {
    regulate_time(f.time.map(|v| v.unwrap_or(0.0)), overflow)
}

/// The ISO calendar's DateAdd: years and months (the day regulated), then
/// weeks and days.
pub(crate) fn date_add(d: IsoDate, dd: &[f64; 4], overflow: Overflow) -> Result<IsoDate> {
    let (y, m) = iso::balance_year_month(d.year + dd[0] as i64, d.month as i64 + dd[1] as i64);
    let intermediate = regulate_iso_date(y as f64, m as f64, d.day as f64, overflow)?;
    let days = dd[3] as i64 + 7 * dd[2] as i64;
    // (Far out of range: an error before the arithmetic overflows.)
    if days.abs() > 1_000_000_000_000 || intermediate.year.abs() > 1_000_000_000 {
        return Err(range("date is out of range"));
    }
    check_date(iso::add_days(intermediate, days))
}

/// Whether adding `years` and `months` to `base` passes `target` in
/// direction `sign` (ISODateSurpasses, its years and months part).
fn surpasses(sign: i64, base: IsoDate, target: IsoDate, years: i64, months: i64) -> bool {
    let cmp = |y: i64, m: i64, d: i64| -> bool {
        let t = (target.year, target.month as i64, target.day as i64);
        let c = (y, m, d).cmp(&t);
        sign * (c as i64) > 0
    };
    let y0 = base.year + years;
    if cmp(y0, base.month as i64, base.day as i64) {
        return true;
    }
    if months == 0 {
        return false;
    }
    let (y, m) = iso::balance_year_month(y0, base.month as i64 + months);
    cmp(y, m, base.day as i64)
}

/// The ISO calendar's DateUntil: `[years, months, weeks, days]` from `one`
/// to `two`, the largest unit `largest`.
pub(crate) fn date_until(one: IsoDate, two: IsoDate, largest: Unit) -> [f64; 4] {
    let sign = match two.cmp(&one) {
        std::cmp::Ordering::Equal => return [0.0; 4],
        std::cmp::Ordering::Greater => 1,
        std::cmp::Ordering::Less => -1,
    };
    let (mut years, mut months) = (0i64, 0i64);
    if largest <= Unit::Month {
        let mut candidate = two.year - one.year;
        if candidate != 0 {
            candidate -= sign;
        }
        while !surpasses(sign, one, two, candidate, 0) {
            years = candidate;
            candidate += sign;
        }
        let mut candidate = sign;
        while !surpasses(sign, one, two, years, candidate) {
            months = candidate;
            candidate += sign;
        }
        if largest == Unit::Month {
            months += years * 12;
            years = 0;
        }
    }
    let (y, m) = iso::balance_year_month(one.year + years, one.month as i64 + months);
    let day = (one.day as i64).min(iso::days_in_month(y, m));
    let mut days = iso::date_epoch_days(two) - iso::epoch_days(y, m, day);
    let mut weeks = 0;
    if largest == Unit::Week {
        weeks = days / 7;
        days %= 7;
    }
    [years as f64, months as f64, weeks as f64, days as f64]
}

// ---- Conversions to records ----

/// ToTemporalDate.
pub(crate) fn to_date(vm: &mut Vm, item: Value, options: Value) -> Result<IsoDate> {
    if is_object(item) {
        match slots(item) {
            Some(Slots::Date(d)) => {
                let o = options_object(vm, options)?;
                overflow(vm, o)?;
                return Ok(d);
            }
            Some(Slots::Zoned(ns, z)) => {
                let dt = zone(vm, z).date_time_at(ns);
                let o = options_object(vm, options)?;
                overflow(vm, o)?;
                return Ok(dt.date);
            }
            Some(Slots::DateTime(dt)) => {
                let o = options_object(vm, options)?;
                overflow(vm, o)?;
                return Ok(dt.date);
            }
            _ => {}
        }
        calendar_with_iso_default(vm, item)?;
        let f = prepare(vm, item, DATE_FIELDS, Required::Some(&[]))?;
        let o = options_object(vm, options)?;
        let ov = overflow(vm, o)?;
        return date_from_fields(&f, ov);
    }
    let s = require_string(item)?;
    let r = parse::parse_date_time(&s).map_err(parse_err)?;
    if r.z {
        return Err(range("Z designator not supported for PlainDate"));
    }
    canonicalize_calendar(r.calendar.as_deref().unwrap_or("iso8601"))?;
    let o = options_object(vm, options)?;
    overflow(vm, o)?;
    check_date(r.date())
}

/// ToTemporalDateTime.
pub(crate) fn to_date_time(vm: &mut Vm, item: Value, options: Value) -> Result<DateTime> {
    if is_object(item) {
        match slots(item) {
            Some(Slots::DateTime(dt)) => {
                let o = options_object(vm, options)?;
                overflow(vm, o)?;
                return Ok(dt);
            }
            Some(Slots::Zoned(ns, z)) => {
                let dt = zone(vm, z).date_time_at(ns);
                let o = options_object(vm, options)?;
                overflow(vm, o)?;
                return Ok(dt);
            }
            Some(Slots::Date(d)) => {
                let o = options_object(vm, options)?;
                overflow(vm, o)?;
                return Ok(DateTime { date: d, time: Time::MIDNIGHT });
            }
            _ => {}
        }
        calendar_with_iso_default(vm, item)?;
        let which: Vec<Field> = DATE_FIELDS.iter().chain(TIME_FIELDS).copied().collect();
        let f = prepare(vm, item, &which, Required::Some(&[]))?;
        let o = options_object(vm, options)?;
        let ov = overflow(vm, o)?;
        let date = date_from_fields(&f, ov)?;
        let time = time_from_fields(&f, ov)?;
        return check_date_time(DateTime { date, time });
    }
    let s = require_string(item)?;
    let r = parse::parse_date_time(&s).map_err(parse_err)?;
    if r.z {
        return Err(range("Z designator not supported for PlainDateTime"));
    }
    canonicalize_calendar(r.calendar.as_deref().unwrap_or("iso8601"))?;
    let o = options_object(vm, options)?;
    overflow(vm, o)?;
    check_date_time(DateTime { date: r.date(), time: r.time.unwrap_or(Time::MIDNIGHT) })
}

/// ToTemporalTime.
pub(crate) fn to_time(vm: &mut Vm, item: Value, options: Value) -> Result<Time> {
    if is_object(item) {
        let t = match slots(item) {
            Some(Slots::Time(t)) => Some(t),
            Some(Slots::DateTime(dt)) => Some(dt.time),
            Some(Slots::Zoned(ns, z)) => Some(zone(vm, z).date_time_at(ns).time),
            _ => None,
        };
        if let Some(t) = t {
            let o = options_object(vm, options)?;
            overflow(vm, o)?;
            return Ok(t);
        }
        let rec = time_record(vm, item, false)?;
        let o = options_object(vm, options)?;
        let ov = overflow(vm, o)?;
        return regulate_time(rec.map(|v| v.unwrap_or(0.0)), ov);
    }
    let s = require_string(item)?;
    // (A calendar annotation is ignored, even one not known.)
    let (t, _) = parse::parse_time(&s).map_err(parse_err)?;
    let o = options_object(vm, options)?;
    overflow(vm, o)?;
    Ok(t)
}

/// ToTimeRecordOrMidnight.
pub(crate) fn to_time_or_midnight(vm: &mut Vm, item: Value) -> Result<Time> {
    if item.is_undefined() { Ok(Time::MIDNIGHT) } else { to_time(vm, item, Value::UNDEFINED) }
}

/// ToTemporalYearMonth.
pub(crate) fn to_year_month(vm: &mut Vm, item: Value, options: Value) -> Result<IsoDate> {
    if is_object(item) {
        if let Some(Slots::YearMonth(d)) = slots(item) {
            let o = options_object(vm, options)?;
            overflow(vm, o)?;
            return Ok(d);
        }
        calendar_with_iso_default(vm, item)?;
        let f = prepare(vm, item, &[Field::Year, Field::Month, Field::MonthCode], Required::Some(&[]))?;
        let o = options_object(vm, options)?;
        let ov = overflow(vm, o)?;
        return year_month_from_fields(&f, ov);
    }
    let s = require_string(item)?;
    let (year, month, day, calendar) = parse::parse_year_month(&s).map_err(parse_err)?;
    canonicalize_calendar(calendar.as_deref().unwrap_or("iso8601"))?;
    let o = options_object(vm, options)?;
    overflow(vm, o)?;
    check_year_month(IsoDate::new(year, month as i64, day as i64))?;
    Ok(IsoDate::new(year, month as i64, 1))
}

/// ToTemporalMonthDay.
pub(crate) fn to_month_day(vm: &mut Vm, item: Value, options: Value) -> Result<IsoDate> {
    if is_object(item) {
        if let Some(Slots::MonthDay(d)) = slots(item) {
            let o = options_object(vm, options)?;
            overflow(vm, o)?;
            return Ok(d);
        }
        if !slots(item).is_some_and(|s| s.has_calendar()) {
            let c = get(vm, item, "calendar")?;
            if !c.is_undefined() {
                to_calendar(vm, c)?;
            }
        }
        let f = prepare(vm, item, DATE_FIELDS, Required::Some(&[]))?;
        let o = options_object(vm, options)?;
        let ov = overflow(vm, o)?;
        return month_day_from_fields(&f, ov);
    }
    let s = require_string(item)?;
    let (month, day, ref_year, calendar) = parse::parse_month_day(&s).map_err(parse_err)?;
    canonicalize_calendar(calendar.as_deref().unwrap_or("iso8601"))?;
    let o = options_object(vm, options)?;
    overflow(vm, o)?;
    // (The ISO calendar's reference year is 1972, whatever the string's.)
    let _ = ref_year;
    Ok(IsoDate::new(1972, month as i64, day as i64))
}

/// ToTemporalInstant: an exact time.
pub(crate) fn to_instant(vm: &mut Vm, item: Value) -> Result<i128> {
    let mut item = item;
    if is_object(item) {
        match slots(item) {
            Some(Slots::Instant(ns) | Slots::Zoned(ns, _)) => return Ok(ns),
            _ => item = to_primitive_string(vm, item)?,
        }
    }
    let s = require_string(item)?;
    let r = parse::parse_date_time(&s).map_err(parse_err)?;
    if !r.z && r.offset.is_none() {
        return Err(range("Temporal.Instant requires a time zone offset"));
    }
    let Some(time) = r.time else { return Err(range("Temporal.Instant requires a time")) };
    let offset = if r.z { 0 } else { r.offset.unwrap().ns };
    let balanced = iso::balance_date_time(r.date(), time.nanos() as i128 - offset as i128);
    if !iso::days_in_range(balanced.date) {
        return Err(range("date/time value is outside of supported range"));
    }
    check_epoch_ns(iso::utc_epoch_ns(balanced))
}

/// ToTemporalZonedDateTime: an exact time and a time zone.
pub(crate) fn to_zoned(vm: &mut Vm, item: Value, options: Value) -> Result<(i128, u32)> {
    let (date, time, z, offset, z_flag, match_minutes, d, offset_opt);
    if is_object(item) {
        if let Some(Slots::Zoned(ns, zz)) = slots(item) {
            let o = options_object(vm, options)?;
            disambiguation(vm, o)?;
            offset_option(vm, o, OffsetOption::Reject)?;
            overflow(vm, o)?;
            return Ok((ns, zz));
        }
        calendar_with_iso_default(vm, item)?;
        let which: Vec<Field> =
            DATE_FIELDS.iter().chain(TIME_FIELDS).copied().chain([Field::Offset, Field::TimeZone]).collect();
        let f = prepare(vm, item, &which, Required::Some(&[Field::TimeZone]))?;
        let o = options_object(vm, options)?;
        d = disambiguation(vm, o)?;
        offset_opt = offset_option(vm, o, OffsetOption::Reject)?;
        let ov = overflow(vm, o)?;
        date = date_from_fields(&f, ov)?;
        time = Some(time_from_fields(&f, ov)?);
        z = f.time_zone.unwrap();
        offset = f.offset.as_deref().map(|s| parse::parse_offset(s).unwrap());
        z_flag = false;
        match_minutes = false;
    } else {
        let s = require_string(item)?;
        let r = parse::parse_date_time(&s).map_err(parse_err)?;
        let Some(annotation) = r.tz.clone() else {
            return Err(range("Temporal.ZonedDateTime requires a time zone ID in brackets"));
        };
        z = time_zone_from_identifier(vm, &annotation)?;
        canonicalize_calendar(r.calendar.as_deref().unwrap_or("iso8601"))?;
        z_flag = r.z;
        offset = r.offset;
        match_minutes = !r.offset.is_some_and(|o| o.sub_minute);
        let o = options_object(vm, options)?;
        d = disambiguation(vm, o)?;
        offset_opt = offset_option(vm, o, OffsetOption::Reject)?;
        overflow(vm, o)?;
        date = r.date();
        time = r.time;
    }
    let behaviour = if z_flag {
        OffsetBehaviour::Exact
    } else if offset.is_none() {
        OffsetBehaviour::Wall
    } else {
        OffsetBehaviour::Option
    };
    let offset_ns = if behaviour == OffsetBehaviour::Option { offset.unwrap().ns } else { 0 };
    let ns = interpret_offset(vm, date, time, behaviour, offset_ns, z, d, offset_opt, match_minutes)?;
    Ok((ns, z))
}
