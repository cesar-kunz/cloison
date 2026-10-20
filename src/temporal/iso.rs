//! The ISO 8601 calendar's arithmetic, exact times and time durations as
//! nanoseconds (`i128`), and rounding: Temporal's abstract operations that
//! touch no object.

use std::cmp::Ordering;

pub const NS_PER_DAY: i128 = 86_400_000_000_000;
/// The extent of exact times: 10⁸ days either side of the epoch.
pub const NS_MAX: i128 = 100_000_000 * NS_PER_DAY;
pub const NS_MIN: i128 = -NS_MAX;
/// The largest time duration: 2⁵³ - 1 seconds and 999999999 nanoseconds.
pub const TIME_DURATION_MAX: i128 = 9_007_199_254_740_991_999_999_999;

/// A date of the proleptic Gregorian calendar (a year far out of
/// Temporal's range too, before it is checked).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct IsoDate {
    pub year: i64,
    pub month: u8,
    pub day: u8,
}

/// A wall-clock time.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct Time {
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
    pub millisecond: u16,
    pub microsecond: u16,
    pub nanosecond: u16,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub struct DateTime {
    pub date: IsoDate,
    pub time: Time,
}

impl IsoDate {
    pub fn new(year: i64, month: i64, day: i64) -> IsoDate {
        IsoDate { year, month: month as u8, day: day as u8 }
    }
}

impl Time {
    pub const MIDNIGHT: Time = Time { hour: 0, minute: 0, second: 0, millisecond: 0, microsecond: 0, nanosecond: 0 };
    pub const NOON: Time = Time { hour: 12, ..Time::MIDNIGHT };

    /// Nanoseconds since midnight.
    pub fn nanos(&self) -> i64 {
        ((((self.hour as i64 * 60 + self.minute as i64) * 60 + self.second as i64) * 1000 + self.millisecond as i64) * 1000
            + self.microsecond as i64)
            * 1000
            + self.nanosecond as i64
    }

    /// The time `ns` nanoseconds after midnight (`0 <= ns <` a day).
    pub fn from_nanos(ns: i64) -> Time {
        Time {
            hour: (ns / 3_600_000_000_000) as u8,
            minute: (ns / 60_000_000_000 % 60) as u8,
            second: (ns / 1_000_000_000 % 60) as u8,
            millisecond: (ns / 1_000_000 % 1000) as u16,
            microsecond: (ns / 1000 % 1000) as u16,
            nanosecond: (ns % 1000) as u16,
        }
    }

    pub fn fields(&self) -> [i64; 6] {
        [
            self.hour as i64,
            self.minute as i64,
            self.second as i64,
            self.millisecond as i64,
            self.microsecond as i64,
            self.nanosecond as i64,
        ]
    }
}

pub fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

pub fn days_in_month(year: i64, month: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

pub fn days_in_year(year: i64) -> i64 {
    if is_leap(year) { 366 } else { 365 }
}

/// Days from 1970-01-01 to `year-month-day` (`month` 1-based; out of
/// range months and days count on: `days_from_civil`).
pub fn epoch_days(year: i64, month: i64, day: i64) -> i64 {
    let (y, m) = balance_year_month(year, month);
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468 + day - 1
}

/// The date `days` after 1970-01-01 (`civil_from_days`).
pub fn from_epoch_days(days: i64) -> IsoDate {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    IsoDate::new(if m <= 2 { y + 1 } else { y }, m, d)
}

/// BalanceISOYearMonth.
pub fn balance_year_month(year: i64, month: i64) -> (i64, i64) {
    let m = month - 1;
    (year + m.div_euclid(12), m.rem_euclid(12) + 1)
}

/// AddDaysToISODate.
pub fn add_days(date: IsoDate, days: i64) -> IsoDate {
    from_epoch_days(epoch_days(date.year, date.month as i64, date.day as i64) + days)
}

pub fn date_epoch_days(d: IsoDate) -> i64 {
    epoch_days(d.year, d.month as i64, d.day as i64)
}

/// BalanceTime of `ns` nanoseconds (from midnight, any number): the days
/// it carries over, and the time.
pub fn balance_time(ns: i128) -> (i64, Time) {
    (ns.div_euclid(NS_PER_DAY) as i64, Time::from_nanos(ns.rem_euclid(NS_PER_DAY) as i64))
}

/// A time's fields (any integers) as nanoseconds from midnight.
pub fn time_nanos(h: i128, min: i128, s: i128, ms: i128, us: i128, ns: i128) -> i128 {
    ((((h * 60 + min) * 60 + s) * 1000 + ms) * 1000 + us) * 1000 + ns
}

/// BalanceISODateTime: `date` and `ns` nanoseconds from its midnight.
pub fn balance_date_time(date: IsoDate, ns: i128) -> DateTime {
    let (days, time) = balance_time(ns);
    DateTime { date: add_days(date, days), time }
}

/// GetUTCEpochNanoseconds.
pub fn utc_epoch_ns(dt: DateTime) -> i128 {
    date_epoch_days(dt.date) as i128 * NS_PER_DAY + dt.time.nanos() as i128
}

/// The UTC date and time of exact time `ns`.
pub fn from_epoch_ns(ns: i128) -> DateTime {
    let days = ns.div_euclid(NS_PER_DAY);
    DateTime { date: from_epoch_days(days as i64), time: Time::from_nanos(ns.rem_euclid(NS_PER_DAY) as i64) }
}

pub fn valid_epoch_ns(ns: i128) -> bool {
    (NS_MIN..=NS_MAX).contains(&ns)
}

/// ISODateTimeWithinLimits.
pub fn date_time_within_limits(dt: DateTime) -> bool {
    if dt.date.year.abs() > 300_000 {
        return false;
    }
    let ns = utc_epoch_ns(dt);
    ns > NS_MIN - NS_PER_DAY && ns < NS_MAX + NS_PER_DAY
}

/// A date within the limits (at noon).
pub fn date_within_limits(d: IsoDate) -> bool {
    date_time_within_limits(DateTime { date: d, time: Time::NOON })
}

/// ISOYearMonthWithinLimits.
pub fn year_month_within_limits(year: i64, month: i64) -> bool {
    (-271_821..=275_760).contains(&year) && !(year == -271_821 && month < 4) && !(year == 275_760 && month > 9)
}

/// CheckISODaysRange's test: no more than 10⁸ days from the epoch.
pub fn days_in_range(d: IsoDate) -> bool {
    d.year.abs() < 300_000 && date_epoch_days(d).abs() <= 100_000_000
}

/// 1 (Monday) to 7 (Sunday).
pub fn day_of_week(d: IsoDate) -> i64 {
    (date_epoch_days(d) + 3).rem_euclid(7) + 1
}

pub fn day_of_year(d: IsoDate) -> i64 {
    date_epoch_days(d) - epoch_days(d.year, 1, 1) + 1
}

/// The ISO week of the year, and its year.
pub fn week_of_year(d: IsoDate) -> (i64, i64) {
    let doy = day_of_year(d);
    let dow = day_of_week(d);
    let week = (doy - dow + 10) / 7;
    let weeks_in = |y: i64| {
        // A year has 53 weeks if it starts on a Thursday, or a Wednesday
        // in a leap year.
        let jan1 = day_of_week(IsoDate::new(y, 1, 1));
        if jan1 == 4 || (jan1 == 3 && is_leap(y)) { 53 } else { 52 }
    };
    if week < 1 {
        (weeks_in(d.year - 1), d.year - 1)
    } else if week > weeks_in(d.year) {
        (1, d.year + 1)
    } else {
        (week, d.year)
    }
}

// ---- Rounding ----

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RoundingMode {
    Ceil,
    Floor,
    Expand,
    Trunc,
    HalfCeil,
    HalfFloor,
    HalfExpand,
    HalfTrunc,
    HalfEven,
}

impl RoundingMode {
    pub const NAMES: [&'static str; 9] =
        ["ceil", "floor", "expand", "trunc", "halfCeil", "halfFloor", "halfExpand", "halfTrunc", "halfEven"];
    pub const ALL: [RoundingMode; 9] = [
        RoundingMode::Ceil,
        RoundingMode::Floor,
        RoundingMode::Expand,
        RoundingMode::Trunc,
        RoundingMode::HalfCeil,
        RoundingMode::HalfFloor,
        RoundingMode::HalfExpand,
        RoundingMode::HalfTrunc,
        RoundingMode::HalfEven,
    ];

    /// NegateRoundingMode.
    pub fn negate(self) -> RoundingMode {
        match self {
            RoundingMode::Ceil => RoundingMode::Floor,
            RoundingMode::Floor => RoundingMode::Ceil,
            RoundingMode::HalfCeil => RoundingMode::HalfFloor,
            RoundingMode::HalfFloor => RoundingMode::HalfCeil,
            m => m,
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Unsigned {
    Zero,
    Infinity,
    HalfZero,
    HalfInfinity,
    HalfEven,
}

/// GetUnsignedRoundingMode.
fn unsigned(mode: RoundingMode, negative: bool) -> Unsigned {
    use RoundingMode::*;
    match (mode, negative) {
        (Ceil, false) | (Floor, true) | (Expand, _) => Unsigned::Infinity,
        (Ceil, true) | (Floor, false) | (Trunc, _) => Unsigned::Zero,
        (HalfCeil, false) | (HalfFloor, true) | (HalfExpand, _) => Unsigned::HalfInfinity,
        (HalfCeil, true) | (HalfFloor, false) | (HalfTrunc, _) => Unsigned::HalfZero,
        (HalfEven, _) => Unsigned::HalfEven,
    }
}

/// ApplyUnsignedRoundingMode: whether `r2` (rather than `r1`) is the
/// result, `cmp` comparing twice the remainder with the distance between
/// them, `even` whether `r1` is an even multiple.
fn take_upper(cmp: Ordering, even: bool, u: Unsigned) -> bool {
    match u {
        Unsigned::Zero => false,
        Unsigned::Infinity => true,
        _ if cmp == Ordering::Less => false,
        _ if cmp == Ordering::Greater => true,
        Unsigned::HalfZero => false,
        Unsigned::HalfInfinity => true,
        Unsigned::HalfEven => !even,
    }
}

/// RoundNumberToIncrement: `x` to a multiple of `increment`, as `mode`
/// rounds (negative numbers by their magnitude, then negated).
pub fn round_to_increment(x: i128, increment: i128, mode: RoundingMode) -> i128 {
    let q = x / increment;
    let r = x % increment;
    if r == 0 {
        return x;
    }
    let negative = x < 0;
    let r1 = q.abs();
    let cmp = (r.abs() * 2).cmp(&increment);
    let rounded = if take_upper(cmp, r1 % 2 == 0, unsigned(mode, negative)) { r1 + 1 } else { r1 };
    increment * if negative { -rounded } else { rounded }
}

/// RoundNumberToIncrementAsIfPositive (an exact time, which rounds the
/// same either side of the epoch).
pub fn round_as_if_positive(x: i128, increment: i128, mode: RoundingMode) -> i128 {
    let q = x.div_euclid(increment);
    let r = x.rem_euclid(increment);
    if r == 0 {
        return x;
    }
    let cmp = (r * 2).cmp(&increment);
    let up = take_upper(cmp, q % 2 == 0, unsigned(mode, false));
    (if up { q + 1 } else { q }) * increment
}

/// Whether `r1` or `r2` (whose magnitudes bound a value at `numerator /
/// denominator` of the way between them, `r1` an `even` multiple or not)
/// is the rounded value, for a value of sign `sign`.
pub fn round_between(numerator: i128, denominator: i128, even: bool, sign: i64, mode: RoundingMode) -> bool {
    let cmp = (numerator.abs() * 2).cmp(&denominator.abs());
    take_upper(cmp, even, unsigned(mode, sign < 0))
}

// ---- Units ----

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Unit {
    Year,
    Month,
    Week,
    Day,
    Hour,
    Minute,
    Second,
    Millisecond,
    Microsecond,
    Nanosecond,
}

impl Unit {
    pub const ALL: [Unit; 10] = [
        Unit::Year,
        Unit::Month,
        Unit::Week,
        Unit::Day,
        Unit::Hour,
        Unit::Minute,
        Unit::Second,
        Unit::Millisecond,
        Unit::Microsecond,
        Unit::Nanosecond,
    ];
    pub const SINGULAR: [&'static str; 10] =
        ["year", "month", "week", "day", "hour", "minute", "second", "millisecond", "microsecond", "nanosecond"];
    pub const PLURAL: [&'static str; 10] =
        ["years", "months", "weeks", "days", "hours", "minutes", "seconds", "milliseconds", "microseconds", "nanoseconds"];

    pub fn name(self) -> &'static str {
        Unit::SINGULAR[self as usize]
    }

    /// Nanoseconds per unit (a day: 24 hours), for a day or a time unit.
    pub fn nanos(self) -> i128 {
        match self {
            Unit::Day => NS_PER_DAY,
            Unit::Hour => 3_600_000_000_000,
            Unit::Minute => 60_000_000_000,
            Unit::Second => 1_000_000_000,
            Unit::Millisecond => 1_000_000,
            Unit::Microsecond => 1000,
            Unit::Nanosecond => 1,
            _ => 0,
        }
    }

    pub fn is_calendar(self) -> bool {
        self <= Unit::Week
    }

    /// TemporalUnitCategory: a date unit (a day or longer), else a time one.
    pub fn is_date(self) -> bool {
        self <= Unit::Day
    }

    /// LargerOfTwoTemporalUnits.
    pub fn larger(self, other: Unit) -> Unit {
        self.min(other)
    }
}

// ---- Time durations (nanoseconds) ----

/// A time duration's checked value: a RangeError past
/// [`TIME_DURATION_MAX`].
pub fn time_duration(ns: i128) -> Option<i128> {
    (ns.abs() <= TIME_DURATION_MAX).then_some(ns)
}

/// TotalTimeDuration's division: `x / n` as the nearest double (through a
/// decimal of 50 places, as the reference implementation does).
pub fn fdiv(x: i128, n: i128) -> f64 {
    let negative = (x < 0) != (n < 0);
    let (x, n) = (x.unsigned_abs(), n.unsigned_abs());
    let (q, mut r) = (x / n, x % n);
    let mut s = q.to_string();
    if r != 0 {
        s.push('.');
        let mut k = 0;
        while r != 0 && k < 50 {
            r *= 10;
            s.push(char::from(b'0' + (r / n) as u8));
            r %= n;
            k += 1;
        }
    }
    let d: f64 = s.parse().unwrap_or(f64::NAN);
    if negative { -d } else { d }
}

/// ISODateTimeToString's year: four digits, or a sign and six.
pub fn year_string(year: i64) -> String {
    if (0..=9999).contains(&year) {
        format!("{year:04}")
    } else {
        format!("{}{:06}", if year < 0 { '-' } else { '+' }, year.abs())
    }
}

/// Precision of seconds in a string: `minute` (no seconds), or 0-9
/// fractional digits, or as many as needed (`None`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Precision {
    Minute,
    Digits(u8),
    Auto,
}

/// FormatFractionalSeconds.
pub fn fraction_string(subsecond_ns: i64, precision: Precision) -> String {
    match precision {
        Precision::Auto if subsecond_ns == 0 => String::new(),
        Precision::Auto => {
            let s = format!("{subsecond_ns:09}");
            format!(".{}", s.trim_end_matches('0'))
        }
        Precision::Digits(0) | Precision::Minute => String::new(),
        Precision::Digits(n) => format!(".{}", &format!("{subsecond_ns:09}")[..n as usize]),
    }
}

/// FormatTimeString.
pub fn time_string(hour: i64, minute: i64, second: i64, subsecond_ns: i64, precision: Precision) -> String {
    if precision == Precision::Minute {
        return format!("{hour:02}:{minute:02}");
    }
    format!("{hour:02}:{minute:02}:{second:02}{}", fraction_string(subsecond_ns, precision))
}

pub fn date_string(d: IsoDate) -> String {
    format!("{}-{:02}-{:02}", year_string(d.year), d.month, d.day)
}

pub fn time_record_string(t: Time, precision: Precision) -> String {
    let sub = t.millisecond as i64 * 1_000_000 + t.microsecond as i64 * 1000 + t.nanosecond as i64;
    time_string(t.hour as i64, t.minute as i64, t.second as i64, sub, precision)
}

pub fn date_time_string(dt: DateTime, precision: Precision) -> String {
    format!("{}T{}", date_string(dt.date), time_record_string(dt.time, precision))
}

/// FormatUTCOffsetNanoseconds: `±HH:MM`, with seconds and a fraction if
/// it has them.
pub fn offset_string(ns: i64) -> String {
    let sign = if ns < 0 { '-' } else { '+' };
    let a = ns.unsigned_abs() as i64;
    let (h, m, s, sub) = (a / 3_600_000_000_000, a / 60_000_000_000 % 60, a / 1_000_000_000 % 60, a % 1_000_000_000);
    let p = if s == 0 && sub == 0 { Precision::Minute } else { Precision::Auto };
    format!("{sign}{}", time_string(h, m, s, sub, p))
}

/// FormatDateTimeUTCOffsetRounded: to the minute.
pub fn offset_string_rounded(ns: i64) -> String {
    let r = round_to_increment(ns as i128, 60_000_000_000, RoundingMode::HalfExpand) as i64;
    offset_minutes_string(r / 60_000_000_000)
}

/// FormatOffsetTimeZoneIdentifier.
pub fn offset_minutes_string(minutes: i64) -> String {
    let sign = if minutes < 0 { '-' } else { '+' };
    let a = minutes.abs();
    format!("{sign}{:02}:{:02}", a / 60, a % 60)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn civil_days_round_trip() {
        for days in [-100_000_001i64, -719_468, -1, 0, 1, 59, 365, 11_017, 100_000_000] {
            let d = from_epoch_days(days);
            assert_eq!(date_epoch_days(d), days);
        }
        assert_eq!(from_epoch_days(0), IsoDate::new(1970, 1, 1));
        assert_eq!(day_of_week(IsoDate::new(1970, 1, 1)), 4);
        assert_eq!(week_of_year(IsoDate::new(2021, 1, 3)), (53, 2020));
        assert_eq!(week_of_year(IsoDate::new(2019, 12, 30)), (1, 2020));
    }

    #[test]
    fn rounding() {
        assert_eq!(round_to_increment(-15, 10, RoundingMode::HalfExpand), -20);
        assert_eq!(round_to_increment(15, 10, RoundingMode::HalfEven), 20);
        assert_eq!(round_to_increment(25, 10, RoundingMode::HalfEven), 20);
        assert_eq!(round_as_if_positive(-15, 10, RoundingMode::Floor), -20);
        assert_eq!(fdiv(3, 2), 1.5);
        assert_eq!(fdiv(-1, 3), -1.0 / 3.0);
    }
}
