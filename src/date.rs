//! `Date` (ECMA-262 §21.4): a time value (milliseconds since the epoch, an
//! `f64`; NaN for an invalid date) kept in the object's `code` bits (see
//! [`date_value`]). Installed by [`install`]. Local time comes from the C
//! library's `localtime_r` (its `tm_gmtoff`), so it follows the machine's
//! time zone, and the spec's own calendar arithmetic does the rest.

use std::ffi::CStr;

use crate::interp::{call_function, to_number, to_primitive, to_string_units};
use crate::object::{Class, DONT_DELETE, DONT_ENUM, READ_ONLY, as_object, object_value};
use crate::value::Value;
use crate::vm::{Code, Error, NativeFn, Result, Vm, string_units};

unsafe extern "C" {
    /// Loads the local time zone from `TZ` or the system (POSIX doesn't
    /// make `localtime_r` do it).
    fn tzset();
}

pub(crate) fn install(vm: &mut Vm) {
    unsafe { tzset() };
    let g = vm.global;
    let dp = vm.date_prototype;
    let any = crate::jit::Ty::Any;
    let c = vm.define_native(g, "Date", date_constructor, 7, any, true);
    let (a_p, a_c) = (vm.a_prototype, vm.a_constructor);
    vm.add_property_with(c, a_p, object_value(dp), READ_ONLY | DONT_ENUM | DONT_DELETE);
    vm.add_property_with(dp, a_c, object_value(c), DONT_ENUM);
    vm.define_natives(
        c,
        &[("now", date_now as NativeFn, 0), ("parse", date_parse, 1), ("UTC", date_utc, 7)],
    );
    vm.define_natives(
        dp,
        &[
            ("getTime", date_get_time as NativeFn, 0),
            ("valueOf", date_get_time, 0),
            ("getFullYear", get_full_year, 0),
            ("getUTCFullYear", get_utc_full_year, 0),
            ("getMonth", get_month, 0),
            ("getUTCMonth", get_utc_month, 0),
            ("getDate", get_date, 0),
            ("getUTCDate", get_utc_date, 0),
            ("getDay", get_day, 0),
            ("getUTCDay", get_utc_day, 0),
            ("getHours", get_hours, 0),
            ("getUTCHours", get_utc_hours, 0),
            ("getMinutes", get_minutes, 0),
            ("getUTCMinutes", get_utc_minutes, 0),
            ("getSeconds", get_seconds, 0),
            ("getUTCSeconds", get_utc_seconds, 0),
            ("getMilliseconds", get_milliseconds, 0),
            ("getUTCMilliseconds", get_utc_milliseconds, 0),
            ("getTimezoneOffset", get_timezone_offset, 0),
            ("getYear", get_year, 0),
            ("setTime", set_time, 1),
            ("setMilliseconds", set_milliseconds, 1),
            ("setUTCMilliseconds", set_utc_milliseconds, 1),
            ("setSeconds", set_seconds, 2),
            ("setUTCSeconds", set_utc_seconds, 2),
            ("setMinutes", set_minutes, 3),
            ("setUTCMinutes", set_utc_minutes, 3),
            ("setHours", set_hours, 4),
            ("setUTCHours", set_utc_hours, 4),
            ("setDate", set_date, 1),
            ("setUTCDate", set_utc_date, 1),
            ("setMonth", set_month, 2),
            ("setUTCMonth", set_utc_month, 2),
            ("setFullYear", set_full_year, 3),
            ("setUTCFullYear", set_utc_full_year, 3),
            ("setYear", set_year, 1),
            ("toString", to_string, 0),
            ("toDateString", to_date_string, 0),
            ("toTimeString", to_time_string, 0),
            ("toLocaleString", to_string, 0),
            ("toLocaleDateString", to_date_string, 0),
            ("toLocaleTimeString", to_time_string, 0),
            ("toISOString", to_iso_string, 0),
            ("toJSON", to_json, 1),
        ],
    );
    // `toGMTString` is the very function `toUTCString` is (Annex B).
    let f = vm.define_native(dp, "toUTCString", to_utc_string, 0, any, false);
    let a = vm.intern("toGMTString");
    vm.add_property_with(dp, a, object_value(f), DONT_ENUM);
}

// ---- Time values (§21.4.1) ----

const MS_PER_SECOND: f64 = 1000.0;
const MS_PER_MINUTE: f64 = 60_000.0;
const MS_PER_HOUR: f64 = 3_600_000.0;
const MS_PER_DAY: f64 = 86_400_000.0;
/// The largest magnitude of a time value (100 million days).
const MAX_TIME: f64 = 8.64e15;

/// `Day(t)`: the day number of time value `t`.
fn day(t: f64) -> f64 {
    (t / MS_PER_DAY).floor()
}

/// `a modulo b` (the spec's: the remainder with `b`'s sign), never -0.
fn modulo(a: f64, b: f64) -> f64 {
    a.rem_euclid(b) + 0.0
}

/// `TimeWithinDay(t)`: the milliseconds into the day, in `0..MS_PER_DAY`.
fn time_within_day(t: f64) -> f64 {
    modulo(t, MS_PER_DAY)
}

/// Whether year `y` is a leap year.
fn is_leap(y: f64) -> bool {
    y % 4.0 == 0.0 && (y % 100.0 != 0.0 || y % 400.0 == 0.0)
}

/// `DayFromYear(y)`: the day number of January 1 of year `y`.
fn day_from_year(y: f64) -> f64 {
    365.0 * (y - 1970.0) + ((y - 1969.0) / 4.0).floor() - ((y - 1901.0) / 100.0).floor()
        + ((y - 1601.0) / 400.0).floor()
}

/// `YearFromTime(t)`: the year `t` falls in (the largest `y` with
/// `TimeFromYear(y) <= t`).
fn year_from_time(t: f64) -> f64 {
    let mut y = (t / (MS_PER_DAY * 365.2425)).floor() + 1970.0;
    while day_from_year(y) * MS_PER_DAY > t {
        y -= 1.0;
    }
    while day_from_year(y + 1.0) * MS_PER_DAY <= t {
        y += 1.0;
    }
    y
}

/// The day of the year (0 to 365) and the year of time value `t`.
fn year_and_day(t: f64) -> (f64, i32) {
    let y = year_from_time(t);
    (y, (day(t) - day_from_year(y)) as i32)
}

/// The first day of the year of each month, for a common and a leap year.
const MONTH_STARTS: [[i32; 13]; 2] = [
    [0, 31, 59, 90, 120, 151, 181, 212, 243, 273, 304, 334, 365],
    [0, 31, 60, 91, 121, 152, 182, 213, 244, 274, 305, 335, 366],
];

/// `MonthFromTime(t)` (0 to 11) and `DateFromTime(t)` (1 to 31).
fn month_and_date(t: f64) -> (f64, f64) {
    let (y, d) = year_and_day(t);
    let starts = &MONTH_STARTS[is_leap(y) as usize];
    let m = starts.iter().rposition(|&s| s <= d).unwrap_or(0).min(11);
    (m as f64, (d - starts[m] + 1) as f64)
}

/// `MonthFromTime(t)`: 0 (January) to 11.
fn month_from_time(t: f64) -> f64 {
    month_and_date(t).0
}

/// `DateFromTime(t)`: the day of the month, 1 to 31.
fn date_from_time(t: f64) -> f64 {
    month_and_date(t).1
}

/// `WeekDay(t)`: 0 (Sunday) to 6.
fn week_day(t: f64) -> f64 {
    modulo(day(t) + 4.0, 7.0)
}

/// `HourFromTime(t)`.
fn hour_from_time(t: f64) -> f64 {
    modulo((t / MS_PER_HOUR).floor(), 24.0)
}

/// `MinFromTime(t)`.
fn min_from_time(t: f64) -> f64 {
    modulo((t / MS_PER_MINUTE).floor(), 60.0)
}

/// `SecFromTime(t)`.
fn sec_from_time(t: f64) -> f64 {
    modulo((t / MS_PER_SECOND).floor(), 60.0)
}

/// `msFromTime(t)`.
fn ms_from_time(t: f64) -> f64 {
    modulo(t, MS_PER_SECOND)
}

/// ToIntegerOrInfinity: NaN to 0, else truncated.
fn to_integer(d: f64) -> f64 {
    if d.is_nan() { 0.0 } else { d.trunc() }
}

/// `MakeTime(hour, min, sec, ms)`: a time within a day (unbounded), NaN
/// for a non-finite part.
fn make_time(h: f64, m: f64, s: f64, ms: f64) -> f64 {
    if !(h.is_finite() && m.is_finite() && s.is_finite() && ms.is_finite()) {
        return f64::NAN;
    }
    let (h, m, s, ms) = (to_integer(h), to_integer(m), to_integer(s), to_integer(ms));
    ((h * MS_PER_HOUR + m * MS_PER_MINUTE) + s * MS_PER_SECOND) + ms
}

/// `MakeDay(year, month, date)`: the day number of the date; the month may
/// be out of 0..12 (years carry) and the date out of the month.
fn make_day(year: f64, month: f64, date: f64) -> f64 {
    if !(year.is_finite() && month.is_finite() && date.is_finite()) {
        return f64::NAN;
    }
    let (y, m, dt) = (to_integer(year), to_integer(month), to_integer(date));
    let ym = y + (m / 12.0).floor();
    // Beyond any time value's year: no day number has it (and `f64`
    // arithmetic on it would lose integers).
    if !ym.is_finite() || ym.abs() > 1e9 {
        return f64::NAN;
    }
    let mn = m.rem_euclid(12.0) as usize;
    let first = day_from_year(ym) + MONTH_STARTS[is_leap(ym) as usize][mn] as f64;
    first + dt - 1.0
}

/// `MakeDate(day, time)`: the time value of day `day` at `time` into it.
fn make_date(day: f64, time: f64) -> f64 {
    if !(day.is_finite() && time.is_finite()) {
        return f64::NAN;
    }
    let tv = day * MS_PER_DAY + time;
    if tv.is_finite() { tv } else { f64::NAN }
}

/// `MakeFullYear(y)`: years 0 to 99 are 1900 to 1999.
fn make_full_year(y: f64) -> f64 {
    if y.is_nan() {
        return f64::NAN;
    }
    let yi = to_integer(y);
    if (0.0..=99.0).contains(&yi) { 1900.0 + yi } else { yi }
}

/// `TimeClip(t)`: `t` as an integral time value in range, else NaN.
fn time_clip(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > MAX_TIME {
        return f64::NAN;
    }
    t.trunc() + 0.0 // (+0.0: no -0)
}

// ---- Local time ----

/// The local time zone's offset from UTC at UTC time `t` (ms), and the
/// zone's abbreviation then. A time the C library can't convert (far out
/// of its range) is given the offset of an equivalent year.
fn local_tza(t: f64) -> (f64, String) {
    let secs = (t / MS_PER_SECOND).floor();
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let time: libc::time_t = secs as libc::time_t;
    let got = unsafe { !libc::localtime_r(&time, &mut tm).is_null() };
    if !got {
        let (y, d) = year_and_day(t);
        let yy = equivalent_year(y);
        if yy != y {
            let tt = (day_from_year(yy) + d as f64) * MS_PER_DAY + time_within_day(t);
            return local_tza(tt);
        }
        return (0.0, "UTC".to_string());
    }
    let name = if tm.tm_zone.is_null() {
        String::new()
    } else {
        unsafe { CStr::from_ptr(tm.tm_zone) }.to_string_lossy().into_owned()
    };
    (tm.tm_gmtoff as f64 * MS_PER_SECOND, name)
}

/// A year in 2000..2028 with the same leap-ness and the same weekday of
/// January 1 as `y` (so the same calendar); `y` itself if `y` is in it.
fn equivalent_year(y: f64) -> f64 {
    if (2000.0..2028.0).contains(&y) {
        return y;
    }
    let leap = is_leap(y);
    let wd = (day_from_year(y) + 4.0).rem_euclid(7.0);
    (2000..2028)
        .map(|yy| yy as f64)
        .find(|&yy| is_leap(yy) == leap && (day_from_year(yy) + 4.0).rem_euclid(7.0) == wd)
        .unwrap_or(2000.0)
}

/// `LocalTime(t)`: UTC time value `t` as a local one.
fn local_time(t: f64) -> f64 {
    t + local_tza(t).0
}

/// `UTC(t)`: local time value `t` as a UTC one (the offset at `t` taken
/// as if UTC, then refined once: right except at a zone transition).
/// NaN for a `t` no offset (less than a day) brings into TimeClip's range,
/// as what it gives goes through TimeClip: `year_from_time` of a year past
/// 2^53 would never end.
fn utc(t: f64) -> f64 {
    if !t.is_finite() || t.abs() > MAX_TIME + MS_PER_DAY {
        return f64::NAN;
    }
    let guess = t - local_tza(t).0;
    t - local_tza(guess).0
}

// ---- Helpers ----

/// Milliseconds since the epoch, now.
pub(crate) fn now_ms() -> f64 {
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH);
    t.map_or(0.0, |d| d.as_millis() as f64)
}

/// The time value of `v` if it is a `Date`.
pub(crate) fn date_value(v: Value) -> Option<f64> {
    let o = v.is_cell().then(|| as_object(v))?;
    unsafe { ((*o).class == Class::Date).then(|| f64::from_bits((*o).code as u64)) }
}

/// `thisTimeValue(this)`: a TypeError unless `this` is a `Date`.
/// `Date.prototype[Symbol.toPrimitive](hint)`: OrdinaryToPrimitive,
/// `toString` first for "string" and "default", `valueOf` for "number".
pub(crate) fn date_to_primitive(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    if !crate::props::is_object(this) {
        return Err(Error::type_error("Date.prototype[Symbol.toPrimitive] called on a non-object"));
    }
    let hint = a.first().and_then(|&h| crate::vm::string_units(h)).map(String::from_utf16_lossy);
    let order = match hint.as_deref() {
        Some("string" | "default") => ["toString", "valueOf"],
        Some("number") => ["valueOf", "toString"],
        _ => return Err(Error::type_error("Invalid hint")),
    };
    for name in order {
        let a = vm.intern(name);
        let m = vm.get(this, a)?;
        if crate::props::is_callable(m) {
            let r = crate::interp::call_function(vm, m, this, &[])?;
            if !crate::props::is_object(r) {
                return Ok(r);
            }
        }
    }
    Err(Error::type_error("Cannot convert object to primitive value"))
}

fn this_time(this: Value) -> Result<f64> {
    date_value(this).ok_or_else(|| Error::type_error("this is not a Date object"))
}

/// Set the time value of the `Date` `this` (checked by the caller).
fn set_date_value(this: Value, t: f64) {
    unsafe { (*as_object(this)).code = t.to_bits() as *const Code };
}

/// ToNumber, with what its `valueOf` threw.
fn to_num(vm: &mut Vm, v: Value) -> Result<f64> {
    let d = to_number(v);
    if vm.conv_thrown { Err(vm.take_conv_error()) } else { Ok(d) }
}

fn arg(a: &[Value], i: usize) -> Value {
    a.get(i).copied().unwrap_or(Value::UNDEFINED)
}

fn num(d: f64) -> Value {
    Value::number(d)
}

fn str_value(vm: &mut Vm, s: &str) -> Value {
    let u: Vec<u16> = s.encode_utf16().collect();
    vm.new_string(&u)
}

/// A new `Date` of time value `t`.
fn new_date(vm: &mut Vm, t: f64) -> Value {
    let dp = object_value(vm.date_prototype);
    let o = vm.new_object_with(Class::Date, dp, 0);
    unsafe { (*o).code = t.to_bits() as *const Code };
    object_value(o)
}

// ---- The constructor and its functions ----

/// `new Date()` (now), `new Date(value)` (a `Date`'s time, a string parsed,
/// else a number), `new Date(year, month[, date, hours, minutes, seconds,
/// ms])` (local time). Called as a function: the string of now.
fn date_constructor(vm: &mut Vm, _: Value, args: &[Value]) -> Result<Value> {
    if !vm.is_construct() {
        let s = date_string(now_ms());
        return Ok(str_value(vm, &s));
    }
    let t = match args {
        [] => now_ms(),
        [v] => match date_value(*v) {
            Some(t) => t,
            None => {
                let p = to_primitive(*v);
                if vm.conv_thrown {
                    return Err(vm.take_conv_error());
                }
                match string_units(p) {
                    Some(u) => parse(u),
                    None => to_num(vm, p)?,
                }
            }
        },
        _ => utc(fields_date(vm, args)?),
    };
    Ok(new_date(vm, time_clip(t)))
}

/// The time value of `new Date(y, m, …)` and `Date.UTC(y[, m, …])` from the
/// arguments: year, month, date, hours, minutes, seconds, ms, missing ones
/// 0 (the date 1). Each is coerced, in order.
fn fields_date(vm: &mut Vm, args: &[Value]) -> Result<f64> {
    let mut f = [f64::NAN, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0];
    for (i, v) in args.iter().take(7).enumerate() {
        f[i] = to_num(vm, *v)?;
    }
    let y = make_full_year(f[0]);
    Ok(make_date(make_day(y, f[1], f[2]), make_time(f[3], f[4], f[5], f[6])))
}

/// `Date.now()`.
fn date_now(_: &mut Vm, _: Value, _: &[Value]) -> Result<Value> {
    Ok(num(now_ms()))
}

/// `Date.parse(s)`: the time value of the string `s` (see [`parse`]).
fn date_parse(_: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let u = to_string_units(arg(a, 0));
    Ok(num(parse(&u)))
}

/// `Date.UTC(year[, month, date, hours, minutes, seconds, ms])`: the time
/// value of the fields as UTC.
fn date_utc(vm: &mut Vm, _: Value, a: &[Value]) -> Result<Value> {
    let t = fields_date(vm, a)?;
    Ok(num(time_clip(t)))
}

// ---- Getters ----

/// `Date.prototype.getTime()` and `valueOf()`.
fn date_get_time(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    Ok(num(this_time(this)?))
}

/// A getter: `field` of the time value, local (`local`) or UTC; NaN for an
/// invalid date.
fn get_field(this: Value, local: bool, field: fn(f64) -> f64) -> Result<Value> {
    let t = this_time(this)?;
    if t.is_nan() {
        return Ok(num(f64::NAN));
    }
    Ok(num(field(if local { local_time(t) } else { t })))
}

macro_rules! getters {
    ($($name:ident($local:expr, $field:expr);)*) => {$(
        fn $name(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
            get_field(this, $local, $field)
        }
    )*};
}

getters! {
    get_full_year(true, year_from_time);
    get_utc_full_year(false, year_from_time);
    get_month(true, month_from_time);
    get_utc_month(false, month_from_time);
    get_date(true, date_from_time);
    get_utc_date(false, date_from_time);
    get_day(true, week_day);
    get_utc_day(false, week_day);
    get_hours(true, hour_from_time);
    get_utc_hours(false, hour_from_time);
    get_minutes(true, min_from_time);
    get_utc_minutes(false, min_from_time);
    get_seconds(true, sec_from_time);
    get_utc_seconds(false, sec_from_time);
    get_milliseconds(true, ms_from_time);
    get_utc_milliseconds(false, ms_from_time);
    get_year(true, |t| year_from_time(t) - 1900.0);
}

/// `Date.prototype.getTimezoneOffset()`: minutes UTC is ahead of local
/// time at this date.
fn get_timezone_offset(_: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    if t.is_nan() {
        return Ok(num(f64::NAN));
    }
    Ok(num((t - local_time(t)) / MS_PER_MINUTE))
}

// ---- Setters ----

/// `Date.prototype.setTime(t)`.
fn set_time(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    this_time(this)?;
    let t = time_clip(to_num(vm, arg(a, 0))?);
    set_date_value(this, t);
    Ok(num(t))
}

/// A setter of the `n` fields from `first` (0 year, 1 month, 2 date, 3
/// hours, 4 minutes, 5 seconds, 6 ms) of the date's local (`local`) or UTC
/// time: the arguments given replace those fields (all coerced first, in
/// order), the rest are kept. An invalid date stays so, except for the
/// year setters, which start from the epoch.
fn set_fields(vm: &mut Vm, this: Value, a: &[Value], first: usize, n: usize, local: bool) -> Result<Value> {
    let t = this_time(this)?;
    let mut given = [f64::NAN; 7];
    let k = a.len().clamp(1, n);
    for i in 0..k {
        given[first + i] = to_num(vm, arg(a, i))?;
    }
    let t = if t.is_nan() {
        if first > 0 {
            return Ok(num(f64::NAN));
        }
        0.0
    } else if local {
        local_time(t)
    } else {
        t
    };
    let (month, date) = month_and_date(t);
    let mut f = [
        year_from_time(t),
        month,
        date,
        hour_from_time(t),
        min_from_time(t),
        sec_from_time(t),
        ms_from_time(t),
    ];
    f[first..first + k].copy_from_slice(&given[first..first + k]);
    let d = make_date(make_day(f[0], f[1], f[2]), make_time(f[3], f[4], f[5], f[6]));
    let u = time_clip(if local { utc(d) } else { d });
    set_date_value(this, u);
    Ok(num(u))
}

macro_rules! setters {
    ($($name:ident($first:expr, $n:expr, $local:expr);)*) => {$(
        fn $name(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
            set_fields(vm, this, a, $first, $n, $local)
        }
    )*};
}

setters! {
    set_milliseconds(6, 1, true);
    set_utc_milliseconds(6, 1, false);
    set_seconds(5, 2, true);
    set_utc_seconds(5, 2, false);
    set_minutes(4, 3, true);
    set_utc_minutes(4, 3, false);
    set_hours(3, 4, true);
    set_utc_hours(3, 4, false);
    set_date(2, 1, true);
    set_utc_date(2, 1, false);
    set_month(1, 2, true);
    set_utc_month(1, 2, false);
    set_full_year(0, 3, true);
    set_utc_full_year(0, 3, false);
}

/// `Date.prototype.setYear(y)` (Annex B): the full year, years 0 to 99
/// being 1900 to 1999.
fn set_year(vm: &mut Vm, this: Value, a: &[Value]) -> Result<Value> {
    let t = this_time(this)?;
    let y = to_num(vm, arg(a, 0))?;
    let t = if t.is_nan() { 0.0 } else { local_time(t) };
    let yyyy = make_full_year(y);
    if yyyy.is_nan() {
        set_date_value(this, f64::NAN);
        return Ok(num(f64::NAN));
    }
    let (month, date) = month_and_date(t);
    let u = time_clip(utc(make_date(make_day(yyyy, month, date), time_within_day(t))));
    set_date_value(this, u);
    Ok(num(u))
}

// ---- Strings ----

const DAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// A year, at least four digits, with a sign if negative.
fn year_string(y: f64) -> String {
    format!("{}{:04}", if y < 0.0 { "-" } else { "" }, y.abs())
}

/// `DateString(t)`: `Tue Oct 15 2026`.
fn date_part(t: f64) -> String {
    let (m, d) = month_and_date(t);
    format!(
        "{} {} {:02} {}",
        DAYS[week_day(t) as usize],
        MONTHS[m as usize],
        d,
        year_string(year_from_time(t))
    )
}

/// `TimeString(t)`: `12:34:56 GMT`.
fn time_part(t: f64) -> String {
    format!("{:02}:{:02}:{:02} GMT", hour_from_time(t), min_from_time(t), sec_from_time(t))
}

/// `TimeZoneString(tv)`: `+0200 (CEST)` for the zone at UTC time `tv`.
fn zone_part(tv: f64) -> String {
    let (off, name) = local_tza(tv);
    let minutes = (off / MS_PER_MINUTE).abs().round();
    let s = format!(
        "{}{:02}{:02}",
        if off < 0.0 { "-" } else { "+" },
        (minutes / 60.0).floor(),
        minutes % 60.0
    );
    if name.is_empty() { s } else { format!("{s} ({name})") }
}

/// `ToDateString(tv)`: `toString`'s format, `Invalid Date` for NaN.
fn date_string(tv: f64) -> String {
    if tv.is_nan() {
        return "Invalid Date".to_string();
    }
    let t = local_time(tv);
    format!("{} {}{}", date_part(t), time_part(t), zone_part(tv))
}

/// `Date.prototype.toString()` (and `toLocaleString`).
fn to_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let s = date_string(this_time(this)?);
    Ok(str_value(vm, &s))
}

/// `Date.prototype.toDateString()` (and `toLocaleDateString`).
fn to_date_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let tv = this_time(this)?;
    let s = if tv.is_nan() { "Invalid Date".to_string() } else { date_part(local_time(tv)) };
    Ok(str_value(vm, &s))
}

/// `Date.prototype.toTimeString()` (and `toLocaleTimeString`).
fn to_time_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let tv = this_time(this)?;
    let s = if tv.is_nan() {
        "Invalid Date".to_string()
    } else {
        format!("{}{}", time_part(local_time(tv)), zone_part(tv))
    };
    Ok(str_value(vm, &s))
}

/// `Date.prototype.toUTCString()` (and `toGMTString`): `Tue, 15 Oct 2026
/// 10:34:56 GMT`.
fn to_utc_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let tv = this_time(this)?;
    let s = if tv.is_nan() {
        "Invalid Date".to_string()
    } else {
        let (m, d) = month_and_date(tv);
        format!(
            "{}, {:02} {} {} {}",
            DAYS[week_day(tv) as usize],
            d,
            MONTHS[m as usize],
            year_string(year_from_time(tv)),
            time_part(tv)
        )
    };
    Ok(str_value(vm, &s))
}

/// `Date.prototype.toISOString()`: `2026-10-15T10:34:56.789Z`, the year as
/// `±YYYYYY` outside 0..=9999; a RangeError for an invalid date.
fn to_iso_string(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    let tv = this_time(this)?;
    if tv.is_nan() {
        return Err(Error::range_error("Invalid time value"));
    }
    let y = year_from_time(tv);
    let year = if (0.0..=9999.0).contains(&y) {
        format!("{y:04}")
    } else {
        format!("{}{:06}", if y < 0.0 { "-" } else { "+" }, y.abs())
    };
    let (m, d) = month_and_date(tv);
    let s = format!(
        "{year}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        m + 1.0,
        d,
        hour_from_time(tv),
        min_from_time(tv),
        sec_from_time(tv),
        ms_from_time(tv)
    );
    Ok(str_value(vm, &s))
}

/// `Date.prototype.toJSON(key)`: `this.toISOString()`, or null when
/// `this`'s primitive is a non-finite number. Generic: any object with a
/// `toISOString`.
fn to_json(vm: &mut Vm, this: Value, _: &[Value]) -> Result<Value> {
    if this.is_undefined() || this.is_null() {
        return Err(Error::type_error("Date.prototype.toJSON called on null or undefined"));
    }
    let tv = to_primitive(this);
    if vm.conv_thrown {
        return Err(vm.take_conv_error());
    }
    if tv.is_number() && !tv.as_number().unwrap().is_finite() {
        return Ok(Value::NULL);
    }
    let a = vm.intern("toISOString");
    let f = vm.get(this, a)?;
    call_function(vm, f, this, &[])
}

// ---- Parsing ----

/// The time value of a date string: the ISO format (§21.4.1.32, `Z` or
/// `±HH:mm` offsets, date-only forms UTC and others local), else the
/// formats of `toString` and `toUTCString`; NaN for anything else.
fn parse(u: &[u16]) -> f64 {
    let s = String::from_utf16_lossy(u);
    let s = s.trim();
    time_clip(parse_iso(s).or_else(|| parse_legacy(s)).unwrap_or(f64::NAN))
}

/// Exactly `n` decimal digits at `*i` of `b`, advancing past them.
fn digits(b: &[u8], i: &mut usize, n: usize) -> Option<f64> {
    let d = b.get(*i..*i + n)?;
    if !d.iter().all(u8::is_ascii_digit) {
        return None;
    }
    *i += n;
    Some(d.iter().fold(0.0, |acc, &c| acc * 10.0 + (c - b'0') as f64))
}

/// Consume byte `c` at `*i` of `b`.
fn eat(b: &[u8], i: &mut usize, c: u8) -> bool {
    let hit = b.get(*i) == Some(&c);
    *i += hit as usize;
    hit
}

/// The ISO format: `YYYY[-MM[-DD]][THH:mm[:ss[.sss]][Z|±HH:mm]]`, the
/// year also `±YYYYYY` (not `-000000`). None unless the whole string is
/// one.
fn parse_iso(s: &str) -> Option<f64> {
    let b = s.as_bytes();
    let mut i = 0;
    let year = match b.first() {
        Some(&c @ (b'+' | b'-')) => {
            i = 1;
            let y = digits(b, &mut i, 6)?;
            if c == b'-' {
                if y == 0.0 {
                    return None;
                }
                -y
            } else {
                y
            }
        }
        _ => digits(b, &mut i, 4)?,
    };
    let (mut month, mut date) = (1.0, 1.0);
    if eat(b, &mut i, b'-') {
        month = digits(b, &mut i, 2)?;
        if eat(b, &mut i, b'-') {
            date = digits(b, &mut i, 2)?;
        }
    }
    if !(1.0..=12.0).contains(&month) {
        return None;
    }
    let days = MONTH_STARTS[is_leap(year) as usize];
    let m = month as usize;
    if !(1.0..=(days[m] - days[m - 1]) as f64).contains(&date) {
        return None;
    }
    let (mut h, mut min, mut sec, mut ms) = (0.0, 0.0, 0.0, 0.0);
    let mut offset = None;
    let date_only = !eat(b, &mut i, b'T');
    if !date_only {
        h = digits(b, &mut i, 2)?;
        if !eat(b, &mut i, b':') {
            return None;
        }
        min = digits(b, &mut i, 2)?;
        if eat(b, &mut i, b':') {
            sec = digits(b, &mut i, 2)?;
            if eat(b, &mut i, b'.') {
                // Any number of digits, the first three being milliseconds.
                let start = i;
                while b.get(i).is_some_and(u8::is_ascii_digit) {
                    i += 1;
                }
                if i == start {
                    return None;
                }
                let frac = &b[start..i.min(start + 3)];
                ms = frac.iter().fold(0.0, |acc, &c| acc * 10.0 + (c - b'0') as f64)
                    * 10f64.powi(3 - frac.len() as i32);
            }
        }
        if eat(b, &mut i, b'Z') {
            offset = Some(0.0);
        } else if let Some(&c @ (b'+' | b'-')) = b.get(i) {
            i += 1;
            let oh = digits(b, &mut i, 2)?;
            if !eat(b, &mut i, b':') {
                return None;
            }
            let om = digits(b, &mut i, 2)?;
            if oh > 23.0 || om > 59.0 {
                return None;
            }
            let o = (oh * 60.0 + om) * MS_PER_MINUTE;
            offset = Some(if c == b'-' { -o } else { o });
        }
        let end_of_day = h == 24.0 && min == 0.0 && sec == 0.0 && ms == 0.0;
        if !(h < 24.0 || end_of_day) || min > 59.0 || sec > 59.0 {
            return None;
        }
    }
    if i != b.len() {
        return None;
    }
    let t = make_date(make_day(year, month - 1.0, date), make_time(h, min, sec, ms));
    Some(match offset {
        Some(o) => t - o,
        None if date_only => t,
        None => utc(t),
    })
}

/// The formats of `toString` (`Tue Oct 15 2026 12:34:56 GMT+0200 (CEST)`)
/// and `toUTCString` (`Tue, 15 Oct 2026 10:34:56 GMT`), loosely: weekday
/// names and parenthesized zone names are skipped, the month name, a day,
/// a year and an optional time and `GMT±hhmm` are taken in any order.
/// Without a zone, local time.
fn parse_legacy(s: &str) -> Option<f64> {
    let s = match s.find('(') {
        Some(p) if s.ends_with(')') => &s[..p],
        _ => s,
    };
    let (mut month, mut date, mut year) = (None, None, None);
    let (mut time, mut offset) = (None, None);
    for tok in s.split(|c: char| c.is_whitespace() || c == ',').filter(|t| !t.is_empty()) {
        let lower = tok.to_ascii_lowercase();
        if DAYS.iter().any(|d| d.eq_ignore_ascii_case(tok)) {
            continue;
        }
        if let Some(m) = MONTHS.iter().position(|m| lower.starts_with(&m.to_ascii_lowercase())) {
            month = Some(m as f64);
            continue;
        }
        if let Some(rest) = lower.strip_prefix("gmt").or_else(|| lower.strip_prefix("utc")) {
            if rest.is_empty() {
                offset = Some(0.0);
                continue;
            }
            let b = rest.as_bytes();
            let (mut i, sign) = (1, if b[0] == b'-' { -1.0 } else { 1.0 });
            if !matches!(b[0], b'+' | b'-') {
                return None;
            }
            let hh = digits(b, &mut i, 2)?;
            eat(b, &mut i, b':');
            let mm = digits(b, &mut i, 2).unwrap_or(0.0);
            if i != b.len() {
                return None;
            }
            offset = Some(sign * (hh * 60.0 + mm) * MS_PER_MINUTE);
            continue;
        }
        if tok.contains(':') {
            let mut parts = tok.split(':').map(|p| p.parse::<f64>().ok());
            let h = parts.next().flatten()?;
            let m = parts.next().flatten()?;
            let sec = parts.next().unwrap_or(Some(0.0))?;
            if parts.next().is_some() || h > 24.0 || m > 59.0 || sec > 59.0 {
                return None;
            }
            time = Some(make_time(h, m, sec, 0.0));
            continue;
        }
        let (neg, ds) = match tok.strip_prefix('-') {
            Some(r) => (true, r),
            None => (false, tok.strip_prefix('+').unwrap_or(tok)),
        };
        if ds.is_empty() || !ds.bytes().all(|c| c.is_ascii_digit()) {
            return None;
        }
        let n = ds.parse::<f64>().ok()?;
        // A day is one or two digits; the year any number, signed or not.
        if date.is_none() && !neg && ds.len() <= 2 && (1.0..=31.0).contains(&n) {
            date = Some(n);
        } else if year.is_none() {
            year = Some(if neg { -n } else { n });
        } else {
            return None;
        }
    }
    let t = make_date(make_day(year?, month?, date?), time.unwrap_or(0.0));
    Some(match offset {
        Some(o) => t - o,
        None => utc(t),
    })
}
