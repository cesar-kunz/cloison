//! Temporal's strings (RFC 9557 date-times with ISO 8601 durations): the
//! grammar of the spec's §13.33, parsed by hand. Errors are RangeErrors'
//! messages.

use super::iso::{IsoDate, Time};

pub type R<T> = std::result::Result<T, String>;

/// A UTC offset in a string: its nanoseconds, and whether it was written
/// past the minute (seconds or a fraction).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Offset {
    pub ns: i64,
    pub sub_minute: bool,
}

/// ParseISODateTime's result.
#[derive(Clone, Debug, Default)]
pub struct Parsed {
    pub year: i64,
    pub month: u8,
    pub day: u8,
    /// `None`: no time (a date alone: the start of the day).
    pub time: Option<Time>,
    pub offset: Option<Offset>,
    pub z: bool,
    /// The bracketed time zone annotation.
    pub tz: Option<String>,
    /// The `u-ca` annotation's value.
    pub calendar: Option<String>,
}

impl Parsed {
    pub fn date(&self) -> IsoDate {
        IsoDate { year: self.year, month: self.month, day: self.day }
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }

    fn at(&self, k: usize) -> Option<u8> {
        self.s.get(self.i + k).copied()
    }

    fn eat(&mut self, c: u8) -> bool {
        if self.peek() == Some(c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn done(&self) -> bool {
        self.i == self.s.len()
    }

    /// `n` digits, as a number.
    fn digits(&mut self, n: usize) -> Option<i64> {
        if self.i + n > self.s.len() || !self.s[self.i..self.i + n].iter().all(u8::is_ascii_digit) {
            return None;
        }
        let v = self.s[self.i..self.i + n].iter().fold(0i64, |a, &c| a * 10 + (c - b'0') as i64);
        self.i += n;
        Some(v)
    }

    fn digit_at(&self, k: usize) -> bool {
        self.at(k).is_some_and(|c| c.is_ascii_digit())
    }

    /// A fraction: `.` or `,` and 1 to 9 digits, as nanoseconds.
    fn fraction(&mut self) -> Option<i64> {
        if !matches!(self.peek(), Some(b'.' | b',')) || !self.digit_at(1) {
            return None;
        }
        self.i += 1;
        let start = self.i;
        while self.peek().is_some_and(|c| c.is_ascii_digit()) {
            self.i += 1;
        }
        let n = self.i - start;
        if n > 9 {
            self.i = start + 9;
            return None;
        }
        let mut v = 0i64;
        for k in 0..9 {
            v = v * 10 + if k < n { (self.s[start + k] - b'0') as i64 } else { 0 };
        }
        Some(v)
    }
}

/// A date: `YYYY-MM-DD`, `YYYYMMDD`, with `±YYYYYY` years.
fn date(p: &mut P) -> Option<(i64, u8, u8)> {
    let year = year(p)?;
    let sep = p.eat(b'-');
    let month = p.digits(2)?;
    if sep && !p.eat(b'-') {
        return None;
    }
    let day = p.digits(2)?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month as u8, day as u8))
}

fn year(p: &mut P) -> Option<i64> {
    match p.peek()? {
        c @ (b'+' | b'-') => {
            p.i += 1;
            let y = p.digits(6)?;
            // (`-000000` is no year.)
            if c == b'-' && y == 0 {
                return None;
            }
            Some(if c == b'-' { -y } else { y })
        }
        _ => p.digits(4),
    }
}

/// A time: `HH`, `HH:MM`, `HH:MM:SS.fff`, or without separators; a leap
/// second (60) is 59.
fn time(p: &mut P) -> Option<Time> {
    let hour = p.digits(2)?;
    let (mut minute, mut second, mut fraction) = (0, 0, 0);
    if p.peek() == Some(b':') && p.digit_at(1) {
        p.i += 1;
        minute = p.digits(2)?;
        if p.peek() == Some(b':') && p.digit_at(1) {
            p.i += 1;
            second = p.digits(2)?;
            fraction = p.fraction().unwrap_or(0);
        }
    } else if p.digit_at(0) && p.digit_at(1) {
        minute = p.digits(2)?;
        if p.digit_at(0) && p.digit_at(1) {
            second = p.digits(2)?;
            fraction = p.fraction().unwrap_or(0);
        }
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }
    Some(Time {
        hour: hour as u8,
        minute: minute as u8,
        second: second.min(59) as u8,
        millisecond: (fraction / 1_000_000) as u16,
        microsecond: (fraction / 1000 % 1000) as u16,
        nanosecond: (fraction % 1000) as u16,
    })
}

/// A UTC offset: `±HH`, `±HH:MM`, `±HH:MM:SS.fff`, or without separators.
fn offset(p: &mut P) -> Option<Offset> {
    let sign = match p.peek()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    p.i += 1;
    let hour = p.digits(2)?;
    if hour > 23 {
        return None;
    }
    let (mut minute, mut second, mut fraction, mut sub_minute) = (0, 0, 0, false);
    let sep = p.peek() == Some(b':');
    if sep || p.digit_at(0) {
        if sep {
            p.i += 1;
        }
        minute = p.digits(2)?;
        if minute > 59 {
            return None;
        }
        let more = if sep { p.peek() == Some(b':') } else { p.digit_at(0) };
        if more {
            if sep {
                p.i += 1;
            }
            second = p.digits(2)?;
            if second > 59 {
                return None;
            }
            sub_minute = true;
            fraction = p.fraction().unwrap_or(0);
        }
    }
    let ns = ((hour * 60 + minute) * 60 + second) * 1_000_000_000 + fraction;
    Some(Offset { ns: sign * ns, sub_minute })
}

/// ParseDateTimeUTCOffset: a whole string an offset (with sub-minute
/// precision allowed).
pub fn parse_offset(s: &str) -> R<Offset> {
    let mut p = P { s: s.as_bytes(), i: 0 };
    match offset(&mut p) {
        Some(o) if p.done() => Ok(o),
        _ => Err(format!("invalid time zone offset: {s}")),
    }
}

/// A time zone identifier's text (in brackets, or alone): an offset to
/// the minute, or a name's components.
fn tz_identifier(p: &mut P) -> Option<String> {
    let start = p.i;
    if matches!(p.peek(), Some(b'+' | b'-')) {
        p.i += 1;
        let h = p.digits(2)?;
        if h > 23 {
            return None;
        }
        let sep = p.eat(b':');
        if sep || p.digit_at(0) {
            if p.digits(2)? > 59 {
                return None;
            }
        }
        return Some(String::from_utf8_lossy(&p.s[start..p.i]).into_owned());
    }
    loop {
        let c0 = p.i;
        match p.peek() {
            Some(c) if c.is_ascii_alphabetic() || c == b'.' || c == b'_' => p.i += 1,
            _ => return None,
        }
        while p.peek().is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-' | b'+')) {
            p.i += 1;
        }
        let component = &p.s[c0..p.i];
        if component == b"." || component == b".." {
            return None;
        }
        if !p.eat(b'/') {
            break;
        }
    }
    Some(String::from_utf8_lossy(&p.s[start..p.i]).into_owned())
}

/// The annotations: a time zone first (if one), then `key=value` ones,
/// of which `u-ca` names the calendar (another with a critical flag `!`
/// is an error, as are two `u-ca`s if either is critical).
fn annotations(p: &mut P, allow_tz: bool) -> R<(Option<String>, Option<String>)> {
    let bad = || "invalid annotations".to_string();
    let mut tz = None;
    let mut calendar: Option<(String, bool)> = None;
    let mut first = true;
    while p.peek() == Some(b'[') {
        p.i += 1;
        let critical = p.eat(b'!');
        // A key's characters, then `=`: an annotation; else a time zone.
        let k0 = p.i;
        let is_key = {
            let mut j = p.i;
            while j < p.s.len() && (p.s[j].is_ascii_lowercase() || p.s[j].is_ascii_digit() || matches!(p.s[j], b'_' | b'-')) {
                j += 1;
            }
            j < p.s.len() && p.s[j] == b'=' && j > k0
        };
        if !is_key {
            if !first || !allow_tz {
                return Err(bad());
            }
            tz = Some(tz_identifier(p).ok_or_else(bad)?);
            if !p.eat(b']') {
                return Err(bad());
            }
            first = false;
            continue;
        }
        first = false;
        if !(p.s[k0].is_ascii_lowercase() || p.s[k0] == b'_') {
            return Err(bad());
        }
        while p.peek() != Some(b'=') {
            p.i += 1;
        }
        let key = String::from_utf8_lossy(&p.s[k0..p.i]).into_owned();
        p.i += 1;
        let v0 = p.i;
        loop {
            let c0 = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_alphanumeric()) {
                p.i += 1;
            }
            if p.i == c0 {
                return Err(bad());
            }
            if !p.eat(b'-') {
                break;
            }
        }
        let value = String::from_utf8_lossy(&p.s[v0..p.i]).into_owned();
        if !p.eat(b']') {
            return Err(bad());
        }
        if key == "u-ca" {
            match &calendar {
                None => calendar = Some((value, critical)),
                Some((_, was_critical)) if critical || *was_critical => {
                    return Err("more than one u-ca annotation with a critical flag".into());
                }
                Some(_) => {}
            }
        } else if critical {
            return Err(format!("unrecognized critical annotation: {key}"));
        }
    }
    Ok((tz, calendar.map(|c| c.0)))
}

/// ParseISODateTime: a date, an optional time (with an optional offset
/// or `Z`), an optional time zone annotation, other annotations.
pub fn parse_date_time(s: &str) -> R<Parsed> {
    let err = || format!("invalid RFC 9557 string: {s}");
    let mut p = P { s: s.as_bytes(), i: 0 };
    let (year, month, day) = date(&mut p).ok_or_else(err)?;
    let mut r = Parsed { year, month, day, ..Parsed::default() };
    if matches!(p.peek(), Some(b'T' | b't' | b' ')) && p.digit_at(1) {
        p.i += 1;
        r.time = Some(time(&mut p).ok_or_else(err)?);
        if matches!(p.peek(), Some(b'Z' | b'z')) {
            p.i += 1;
            r.z = true;
        } else if matches!(p.peek(), Some(b'+' | b'-')) {
            r.offset = Some(offset(&mut p).ok_or_else(err)?);
        }
    }
    let (tz, calendar) = annotations(&mut p, true).map_err(|e| format!("{e}: {s}"))?;
    if !p.done() {
        return Err(err());
    }
    r.tz = tz;
    r.calendar = calendar;
    let days = super::iso::days_in_month(year, month as i64);
    if day as i64 > days {
        return Err(err());
    }
    Ok(r)
}

/// ParseTemporalTimeString's grammar: a time alone (`T` optional), with
/// an optional offset or `Z`, and annotations.
struct TimeOnly {
    time: Time,
    designator: bool,
    z: bool,
    offset: Option<Offset>,
    tz: Option<String>,
    calendar: Option<String>,
}

fn time_only(s: &str) -> R<TimeOnly> {
    let err = || format!("invalid time string: {s}");
    let mut p = P { s: s.as_bytes(), i: 0 };
    let designator = matches!(p.peek(), Some(b'T' | b't'));
    if designator {
        p.i += 1;
    }
    let time = time(&mut p).ok_or_else(err)?;
    let (mut z, mut off) = (false, None);
    if matches!(p.peek(), Some(b'Z' | b'z')) {
        p.i += 1;
        z = true;
    } else if matches!(p.peek(), Some(b'+' | b'-')) {
        off = Some(offset(&mut p).ok_or_else(err)?);
    }
    let (tz, calendar) = annotations(&mut p, true).map_err(|e| format!("{e}: {s}"))?;
    if !p.done() {
        return Err(err());
    }
    Ok(TimeOnly { time, designator, z, offset: off, tz, calendar })
}

/// ParseTemporalTimeString: a time (of a date-time too), its calendar
/// annotation. A time alone that could be read as a month and day or a
/// year and month needs its `T`; `Z` is an error.
pub fn parse_time(s: &str) -> R<(Time, Option<String>)> {
    match time_only(s) {
        Ok(t) => {
            if t.z {
                return Err("Z designator not supported for PlainTime".into());
            }
            if !t.designator {
                if let Ok((month, day, _)) = parse_month_day_short(s) {
                    if day as i64 <= super::iso::days_in_month(1972, month as i64) {
                        return Err(format!("ambiguous time string, needs a T: {s}"));
                    }
                }
                if parse_year_month_short(s).is_ok() {
                    return Err(format!("ambiguous time string, needs a T: {s}"));
                }
            }
            Ok((t.time, t.calendar))
        }
        Err(e) => {
            let r = parse_date_time(s).map_err(|_| e)?;
            let Some(t) = r.time else { return Err(format!("time is missing in string: {s}")) };
            if r.z {
                return Err("Z designator not supported for PlainTime".into());
            }
            Ok((t, r.calendar))
        }
    }
}

/// The short year-month form: `YYYY-MM` (`YYYYMM`), annotations.
fn parse_year_month_short(s: &str) -> R<(i64, u8, Option<String>)> {
    let err = || format!("invalid year-month string: {s}");
    let mut p = P { s: s.as_bytes(), i: 0 };
    let y = year(&mut p).ok_or_else(err)?;
    p.eat(b'-');
    let m = p.digits(2).ok_or_else(err)?;
    if !(1..=12).contains(&m) {
        return Err(err());
    }
    let (_, calendar) = annotations(&mut p, true)?;
    if !p.done() {
        return Err(err());
    }
    Ok((y, m as u8, calendar))
}

/// The short month-day form: `--MM-DD` (`MM-DD`, `MMDD`), annotations.
fn parse_month_day_short(s: &str) -> R<(u8, u8, Option<String>)> {
    let err = || format!("invalid month-day string: {s}");
    let mut p = P { s: s.as_bytes(), i: 0 };
    if p.peek() == Some(b'-') {
        if p.at(1) != Some(b'-') {
            return Err(err());
        }
        p.i += 2;
    }
    let m = p.digits(2).ok_or_else(err)?;
    p.eat(b'-');
    let d = p.digits(2).ok_or_else(err)?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return Err(err());
    }
    let (_, calendar) = annotations(&mut p, true)?;
    if !p.done() {
        return Err(err());
    }
    Ok((m as u8, d as u8, calendar))
}

fn check_iso_calendar(calendar: &Option<String>, what: &str) -> R<()> {
    if let Some(c) = calendar {
        if !c.eq_ignore_ascii_case("iso8601") {
            return Err(format!("{what} format is only valid with the iso8601 calendar"));
        }
    }
    Ok(())
}

/// ParseTemporalYearMonthString: the year, month, reference day, and
/// calendar.
pub fn parse_year_month(s: &str) -> R<(i64, u8, u8, Option<String>)> {
    if let Ok((y, m, calendar)) = parse_year_month_short(s) {
        check_iso_calendar(&calendar, "YYYY-MM")?;
        return Ok((y, m, 1, calendar));
    }
    let r = parse_date_time(s)?;
    if r.z {
        return Err("Z designator not supported for PlainYearMonth".into());
    }
    Ok((r.year, r.month, r.day, r.calendar))
}

/// ParseTemporalMonthDayString: the month, day, reference year (if one),
/// and calendar.
pub fn parse_month_day(s: &str) -> R<(u8, u8, Option<i64>, Option<String>)> {
    if let Ok((m, d, calendar)) = parse_month_day_short(s) {
        check_iso_calendar(&calendar, "MM-DD")?;
        if d as i64 > super::iso::days_in_month(1972, m as i64) {
            return Err(format!("invalid month-day string: {s}"));
        }
        return Ok((m, d, None, calendar));
    }
    let r = parse_date_time(s)?;
    if r.z {
        return Err("Z designator not supported for PlainMonthDay".into());
    }
    Ok((r.month, r.day, Some(r.year), r.calendar))
}

/// What a time zone string names: an offset in minutes, or a name.
#[derive(Clone, Debug, PartialEq)]
pub enum TzName {
    Offset(i64),
    Name(String),
}

/// ParseTimeZoneIdentifier: a whole string a time zone identifier.
pub fn parse_tz_identifier(s: &str) -> R<TzName> {
    let mut p = P { s: s.as_bytes(), i: 0 };
    match tz_identifier(&mut p) {
        Some(id) if p.done() => {
            if matches!(s.as_bytes()[0], b'+' | b'-') {
                let o = parse_offset(&id)?;
                Ok(TzName::Offset(o.ns / 60_000_000_000))
            } else {
                Ok(TzName::Name(id))
            }
        }
        _ => {
            if parse_offset(s).is_ok() {
                Err(format!("seconds not allowed in an offset time zone: {s}"))
            } else {
                Err(format!("invalid time zone: {s}"))
            }
        }
    }
}

/// ParseTemporalTimeZoneString: an identifier, or the time zone of an
/// RFC 9557 string (its annotation, else `Z` (UTC), else its offset).
pub fn parse_tz_string(s: &str) -> R<TzName> {
    if let Ok(t) = parse_tz_identifier(s) {
        return Ok(t);
    }
    let bad = || {
        if parse_offset(s).is_ok() {
            format!("seconds not allowed in an offset time zone: {s}")
        } else {
            format!("invalid time zone: {s}")
        }
    };
    let found = if let Ok(r) = parse_date_time(s) {
        Some((r.tz, r.z, r.offset))
    } else if let Ok(t) = time_only(s) {
        Some((t.tz, t.z, t.offset))
    } else {
        None
    };
    match found {
        Some((Some(tz), ..)) => parse_tz_identifier(&tz),
        Some((None, true, _)) => Ok(TzName::Name("UTC".into())),
        Some((None, false, Some(o))) if o.sub_minute => Err(bad()),
        Some((None, false, Some(o))) => {
            let text = crate::temporal::iso::offset_string(o.ns);
            parse_tz_identifier(&text).map_err(|_| bad())
        }
        _ => Err(bad()),
    }
}

/// A duration's fields as parsed (signed, each an integer).
pub type DurationFields = [f64; 10];

/// ParseTemporalDurationString's fields (before RejectDuration).
pub fn parse_duration(s: &str) -> R<DurationFields> {
    let err = || format!("invalid duration: {s}");
    let b = s.as_bytes();
    let mut p = P { s: b, i: 0 };
    let sign = match p.peek() {
        Some(b'-') => {
            p.i += 1;
            -1.0
        }
        Some(b'+') => {
            p.i += 1;
            1.0
        }
        _ => 1.0,
    };
    if !matches!(p.peek(), Some(b'P' | b'p')) {
        return Err(err());
    }
    p.i += 1;
    // A number, its fraction (nanoseconds, if any), its designator.
    let number = |p: &mut P| -> Option<(f64, Option<i64>, u8)> {
        let start = p.i;
        while p.peek().is_some_and(|c| c.is_ascii_digit()) {
            p.i += 1;
        }
        if p.i == start {
            return None;
        }
        let digits = std::str::from_utf8(&p.s[start..p.i]).ok()?;
        let v: f64 = digits.parse().ok()?;
        let fraction = p.fraction();
        let d = p.peek()?.to_ascii_uppercase();
        p.i += 1;
        Some((v, fraction, d))
    };
    let mut f = [0f64; 10];
    let mut any = false;
    let order = *b"YMWD";
    let mut next = 0;
    while p.peek().is_some_and(|c| c.is_ascii_digit()) {
        let (v, fraction, d) = number(&mut p).ok_or_else(err)?;
        let k = order[next..].iter().position(|&c| c == d).ok_or_else(err)? + next;
        if fraction.is_some() {
            return Err(err());
        }
        f[k] = v;
        next = k + 1;
        any = true;
    }
    let mut excess = 0f64;
    if matches!(p.peek(), Some(b'T' | b't')) {
        p.i += 1;
        let order = *b"HMS";
        let mut next = 0;
        let mut fractional = false;
        let mut any_time = false;
        while !p.done() {
            if fractional {
                return Err("only the smallest unit can be fractional".into());
            }
            let (v, fraction, d) = number(&mut p).ok_or_else(err)?;
            let k = order[next..].iter().position(|&c| c == d).ok_or_else(err)? + next;
            f[4 + k] = v;
            if let Some(n) = fraction {
                fractional = true;
                excess = n as f64 * [3600.0, 60.0, 1.0][k];
            }
            next = k + 1;
            any_time = true;
        }
        if !any_time {
            return Err(err());
        }
        any = true;
    }
    if !p.done() || !any {
        return Err(err());
    }
    if f.iter().any(|v| !v.is_finite()) {
        return Err("duration values must be finite".into());
    }
    // The fraction's nanoseconds spread over the smaller units.
    f[9] += excess % 1000.0;
    f[8] += (excess / 1000.0).trunc() % 1000.0;
    f[7] += (excess / 1e6).trunc() % 1000.0;
    f[6] += (excess / 1e9).trunc() % 60.0;
    f[5] += (excess / 60e9).trunc();
    for v in &mut f {
        *v = if *v == 0.0 { 0.0 } else { *v * sign };
    }
    Ok(f)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_times() {
        let r = parse_date_time("2020-01-01T12:34:56.789+01:00[Europe/Vienna][u-ca=iso8601]").unwrap();
        assert_eq!((r.year, r.month, r.day), (2020, 1, 1));
        assert_eq!(r.time.unwrap().millisecond, 789);
        assert_eq!(r.offset.unwrap().ns, 3_600_000_000_000);
        assert_eq!(r.tz.as_deref(), Some("Europe/Vienna"));
        assert_eq!(r.calendar.as_deref(), Some("iso8601"));
        assert!(parse_date_time("+002020-01-01").is_ok());
        assert!(parse_date_time("-000000-01-01").is_err());
        assert!(parse_date_time("2020-01-01[!foo=bar]").is_err());
        assert!(parse_date_time("20200101T1234Z").unwrap().z);
        assert!(parse_time("12:30").is_ok());
        assert!(parse_time("1214").is_err());
        assert!(parse_time("T1214").is_ok());
        assert_eq!(parse_duration("P1Y2M3W4DT5H6M7.5S").unwrap()[6], 7.0);
        assert_eq!(parse_duration("PT1.5H").unwrap()[5], 30.0);
        assert!(parse_duration("PT").is_err());
        assert!(parse_duration("P1YT").is_err());
        assert_eq!(parse_tz_identifier("+05:30").unwrap(), TzName::Offset(330));
        assert_eq!(parse_tz_string("2020-01-01T00:00Z").unwrap(), TzName::Name("UTC".into()));
    }
}
