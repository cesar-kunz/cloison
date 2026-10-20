//! Time zones: UTC offsets, and the IANA database's named zones, read from
//! the system's compiled files (`TZDIR`, else `/usr/share/zoneinfo`).
//!
//! A named zone's identifier is found case-insensitively among the files
//! there and the links `tzdata.zi` lists (a system may have the files of
//! primary zones only); its primary identifier (what `TimeZoneEquals`
//! compares) follows links and symbolic links. A file (TZif, RFC 8536) has
//! its transitions up to some year, and a POSIX TZ rule for after them.

use std::collections::HashMap;
use std::rc::Rc;

use super::iso::{self, DateTime, NS_MAX, NS_MIN, NS_PER_DAY};

const NS_PER_S: i128 = 1_000_000_000;

/// A POSIX TZ rule's date of a transition.
#[derive(Clone, Copy, Debug)]
enum RuleDate {
    /// `Jn`: day `n` (1-365) of the year, February 29 never counted.
    Julian1(i64),
    /// `n`: day `n` (0-365), February 29 counted.
    Julian0(i64),
    /// `Mm.w.d`: weekday `d` (0: Sunday) of week `w` (5: the last) of
    /// month `m`.
    Mwd(i64, i64, i64),
}

#[derive(Clone, Copy, Debug)]
struct Dst {
    /// UTC offset in daylight time (seconds).
    offset: i64,
    start: (RuleDate, i64),
    end: (RuleDate, i64),
}

#[derive(Clone, Copy, Debug)]
struct Rule {
    /// UTC offset in standard time (seconds).
    std: i64,
    dst: Option<Dst>,
}

/// A named zone's data: the offset before its first transition, its
/// transitions (seconds, and the offset from then on: only those that
/// change it), and the rule after the last.
#[derive(Debug)]
pub struct Data {
    initial: i64,
    transitions: Vec<(i64, i64)>,
    rule: Option<Rule>,
}

// ---- TZif ----

fn be32(b: &[u8], at: usize) -> Option<i64> {
    Some(i32::from_be_bytes(b.get(at..at + 4)?.try_into().ok()?) as i64)
}

fn be64(b: &[u8], at: usize) -> Option<i64> {
    Some(i64::from_be_bytes(b.get(at..at + 8)?.try_into().ok()?))
}

/// A TZif file's data (its version 2+ part, if it has one).
fn parse_tzif(b: &[u8]) -> Option<Data> {
    if b.get(..4)? != b"TZif" {
        return None;
    }
    let version = *b.get(4)?;
    let counts = |at: usize| -> Option<[usize; 6]> {
        let mut c = [0usize; 6];
        for (k, v) in c.iter_mut().enumerate() {
            *v = be32(b, at + 20 + 4 * k)? as usize;
        }
        Some(c)
    };
    // [isutcnt, isstdcnt, leapcnt, timecnt, typecnt, charcnt]
    let c1 = counts(0)?;
    let v1_len = c1[3] * 5 + c1[4] * 6 + c1[5] + c1[2] * 8 + c1[1] + c1[0];
    let (start, c, time_size) = if version >= b'2' {
        let h2 = 44 + v1_len;
        (h2 + 44, counts(h2)?, 8)
    } else {
        (44, c1, 4)
    };
    let times_at = start;
    let idx_at = times_at + c[3] * time_size;
    let types_at = idx_at + c[3];
    let mut types = Vec::with_capacity(c[4]);
    for k in 0..c[4] {
        types.push(be32(b, types_at + 6 * k)?);
    }
    let initial = *types.first()?;
    let mut transitions = Vec::new();
    let mut last = initial;
    for k in 0..c[3] {
        let t = if time_size == 8 { be64(b, times_at + 8 * k)? } else { be32(b, times_at + 4 * k)? };
        let off = *types.get(*b.get(idx_at + k)? as usize)?;
        if off != last {
            transitions.push((t, off));
            last = off;
        }
    }
    let data_end = types_at + c[4] * 6 + c[5] + c[2] * (time_size + 4) + c[1] + c[0];
    let rule = if version >= b'2' {
        let footer = b.get(data_end..)?;
        let text = std::str::from_utf8(footer).ok()?.trim_matches('\n');
        parse_posix(text)
    } else {
        None
    };
    Some(Data { initial, transitions, rule })
}

// ---- POSIX TZ rules ----

struct Posix<'a> {
    s: &'a [u8],
    i: usize,
}

impl Posix<'_> {
    fn name(&mut self) -> Option<()> {
        if self.s.get(self.i) == Some(&b'<') {
            while *self.s.get(self.i)? != b'>' {
                self.i += 1;
            }
            self.i += 1;
            return Some(());
        }
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_alphabetic) {
            self.i += 1;
        }
        (self.i - start >= 3).then_some(())
    }

    fn number(&mut self) -> Option<i64> {
        let start = self.i;
        while self.s.get(self.i).is_some_and(u8::is_ascii_digit) {
            self.i += 1;
        }
        std::str::from_utf8(&self.s[start..self.i]).ok()?.parse().ok()
    }

    /// `[+-]hh[:mm[:ss]]`, in seconds.
    fn hms(&mut self) -> Option<i64> {
        let sign = match self.s.get(self.i) {
            Some(b'-') => {
                self.i += 1;
                -1
            }
            Some(b'+') => {
                self.i += 1;
                1
            }
            _ => 1,
        };
        let mut v = self.number()? * 3600;
        if self.s.get(self.i) == Some(&b':') {
            self.i += 1;
            v += self.number()? * 60;
            if self.s.get(self.i) == Some(&b':') {
                self.i += 1;
                v += self.number()?;
            }
        }
        Some(sign * v)
    }

    fn date(&mut self) -> Option<(RuleDate, i64)> {
        let d = match self.s.get(self.i)? {
            b'J' => {
                self.i += 1;
                RuleDate::Julian1(self.number()?)
            }
            b'M' => {
                self.i += 1;
                let m = self.number()?;
                self.i += 1;
                let w = self.number()?;
                self.i += 1;
                RuleDate::Mwd(m, w, self.number()?)
            }
            _ => RuleDate::Julian0(self.number()?),
        };
        let time = if self.s.get(self.i) == Some(&b'/') {
            self.i += 1;
            self.hms()?
        } else {
            7200
        };
        Some((d, time))
    }
}

/// A POSIX TZ string (offsets in it are west of UTC: negated here).
fn parse_posix(text: &str) -> Option<Rule> {
    let mut p = Posix { s: text.as_bytes(), i: 0 };
    p.name()?;
    let std = -p.hms()?;
    if p.i == p.s.len() {
        return Some(Rule { std, dst: None });
    }
    p.name()?;
    let offset = if matches!(p.s.get(p.i), Some(b',') | None) { std + 3600 } else { -p.hms()? };
    if p.s.get(p.i) != Some(&b',') {
        return Some(Rule { std, dst: None });
    }
    p.i += 1;
    let start = p.date()?;
    p.i += 1;
    let end = p.date()?;
    Some(Rule { std, dst: Some(Dst { offset, start, end }) })
}

/// The day of year `year` (since the epoch) a rule's date is.
fn rule_day(year: i64, d: RuleDate) -> i64 {
    let jan1 = iso::epoch_days(year, 1, 1);
    match d {
        RuleDate::Julian1(n) => jan1 + n - 1 + if iso::is_leap(year) && n >= 60 { 1 } else { 0 },
        RuleDate::Julian0(n) => jan1 + n,
        RuleDate::Mwd(m, w, wd) => {
            let first = iso::epoch_days(year, m, 1);
            // (Day 0, 1970-01-01, was a Thursday: weekday 4.)
            let first_wd = (first + 4).rem_euclid(7);
            let mut day = first + (wd - first_wd).rem_euclid(7) + (w - 1) * 7;
            let dim = iso::days_in_month(year, m);
            while day >= first + dim {
                day -= 7;
            }
            day
        }
    }
}

impl Rule {
    /// The rule's two transitions in `year` (UTC seconds), each with the
    /// offset after it, in order.
    fn transitions(&self, year: i64) -> Option<[(i64, i64); 2]> {
        let dst = self.dst?;
        let start = rule_day(year, dst.start.0) * 86400 + dst.start.1 - self.std;
        let end = rule_day(year, dst.end.0) * 86400 + dst.end.1 - dst.offset;
        let (a, b) = ((start, dst.offset), (end, self.std));
        Some(if start <= end { [a, b] } else { [b, a] })
    }

    fn offset_at(&self, t: i64) -> i64 {
        let Some(_) = self.dst else { return self.std };
        let year = iso::from_epoch_days(t.div_euclid(86400)).year;
        // The transition of this year or the one before last before `t`.
        let mut off = self.transitions(year - 1).map_or(self.std, |tr| tr[1].1);
        for y in [year - 1, year] {
            for (when, o) in self.transitions(y).unwrap() {
                if when <= t {
                    off = o;
                }
            }
        }
        off
    }
}

impl Data {
    /// The UTC offset (seconds) at `t` (UTC seconds).
    pub fn offset_at(&self, t: i64) -> i64 {
        match self.transitions.partition_point(|&(when, _)| when <= t) {
            0 => self.initial,
            k if k == self.transitions.len() => match &self.rule {
                Some(r) => r.offset_at(t),
                None => self.transitions[k - 1].1,
            },
            k => self.transitions[k - 1].1,
        }
    }

    /// The rule's transitions after the last listed one, in years
    /// `from..=to`, in order: those that change the offset.
    fn rule_transitions(&self, from: i64, to: i64) -> Vec<i64> {
        let Some(rule) = self.rule.filter(|r| r.dst.is_some()) else { return Vec::new() };
        let last = self.transitions.last().map_or(i64::MIN, |l| l.0);
        let mut out = Vec::new();
        for y in from..=to {
            for (when, o) in rule.transitions(y).unwrap() {
                if when > last && self.offset_at(when - 1) != o {
                    out.push(when);
                }
            }
        }
        out
    }

    /// The first transition after `ns` (an exact time), if any.
    pub fn next_transition(&self, ns: i128) -> Option<i128> {
        let t = ns.div_euclid(NS_PER_S) as i64;
        let k = self.transitions.partition_point(|&(when, _)| when <= t);
        if let Some(&(when, _)) = self.transitions.get(k) {
            return Some(when as i128 * NS_PER_S);
        }
        let year = iso::from_epoch_days(t.div_euclid(86400)).year;
        self.rule_transitions(year - 1, year + 1).into_iter().find(|&w| w > t).map(|w| w as i128 * NS_PER_S)
    }

    /// The last transition before `ns`, if any.
    pub fn previous_transition(&self, ns: i128) -> Option<i128> {
        // (At whole seconds: before `ns` is at or before this one.)
        let t = (ns - 1).div_euclid(NS_PER_S) as i64;
        let year = iso::from_epoch_days(t.div_euclid(86400)).year;
        if let Some(w) = self.rule_transitions(year - 1, year).into_iter().rev().find(|&w| w <= t) {
            return Some(w as i128 * NS_PER_S);
        }
        let k = self.transitions.partition_point(|&(when, _)| when <= t);
        (k > 0).then(|| self.transitions[k - 1].0 as i128 * NS_PER_S)
    }
}

// ---- The registry ----

/// A time zone, as a ZonedDateTime holds it: its identifier (as given,
/// case-normalized), and what it is.
#[derive(Clone)]
pub struct Zone {
    pub id: String,
    pub kind: ZoneKind,
}

#[derive(Clone)]
pub enum ZoneKind {
    /// A fixed offset, in minutes.
    Offset(i64),
    /// A named zone: its primary identifier, its data.
    Named(String, Rc<Data>),
}

impl Zone {
    /// The UTC offset (nanoseconds) at exact time `ns`.
    pub fn offset_ns(&self, ns: i128) -> i64 {
        match &self.kind {
            ZoneKind::Offset(m) => m * 60_000_000_000,
            ZoneKind::Named(_, d) => d.offset_at(ns.div_euclid(NS_PER_S) as i64) * 1_000_000_000,
        }
    }

    /// GetISODateTimeFor.
    pub fn date_time_at(&self, ns: i128) -> DateTime {
        iso::from_epoch_ns(ns + self.offset_ns(ns) as i128)
    }

    /// GetPossibleEpochNanoseconds's candidates for wall time `dt` (none
    /// in a gap, two in an overlap), before they are checked against the
    /// range of exact times.
    pub fn possible(&self, dt: DateTime) -> Vec<i128> {
        let local = iso::utc_epoch_ns(dt);
        match &self.kind {
            ZoneKind::Offset(m) => vec![local - *m as i128 * 60_000_000_000],
            ZoneKind::Named(..) => {
                let before = self.offset_ns((local - NS_PER_DAY).clamp(NS_MIN, NS_MAX));
                let after = self.offset_ns((local + NS_PER_DAY).clamp(NS_MIN, NS_MAX));
                let mut out: Vec<i128> = Vec::new();
                for off in if before == after { vec![before] } else { vec![before, after] } {
                    let ns = local - off as i128;
                    if self.offset_ns(ns) == off && !out.contains(&ns) {
                        out.push(ns);
                    }
                }
                out.sort();
                out
            }
        }
    }

    pub fn next_transition(&self, ns: i128) -> Option<i128> {
        match &self.kind {
            ZoneKind::Offset(_) => None,
            ZoneKind::Named(_, d) => d.next_transition(ns).filter(|&t| t <= NS_MAX),
        }
    }

    pub fn previous_transition(&self, ns: i128) -> Option<i128> {
        match &self.kind {
            ZoneKind::Offset(_) => None,
            ZoneKind::Named(_, d) => d.previous_transition(ns).filter(|&t| t >= NS_MIN),
        }
    }

    /// TimeZoneEquals.
    pub fn equals(&self, other: &Zone) -> bool {
        match (&self.kind, &other.kind) {
            (ZoneKind::Offset(a), ZoneKind::Offset(b)) => a == b,
            (ZoneKind::Named(a, _), ZoneKind::Named(b, _)) => a == b,
            _ => false,
        }
    }
}

/// The named zones available: their identifiers by lowercase (files and
/// links), and the links' targets.
#[derive(Default)]
pub struct Registry {
    dir: String,
    names: HashMap<String, String>,
    links: HashMap<String, String>,
    loaded: HashMap<String, Rc<Data>>,
    scanned: bool,
}

impl Registry {
    fn scan(&mut self) {
        if self.scanned {
            return;
        }
        self.scanned = true;
        self.dir = std::env::var("TZDIR").unwrap_or_else(|_| "/usr/share/zoneinfo".into());
        let dir = self.dir.clone();
        let mut stack = vec![String::new()];
        while let Some(rel) = stack.pop() {
            let Ok(entries) = std::fs::read_dir(format!("{dir}/{rel}")) else { continue };
            for e in entries.flatten() {
                let name = e.file_name().to_string_lossy().into_owned();
                let path = if rel.is_empty() { name.clone() } else { format!("{rel}/{name}") };
                let Ok(meta) = std::fs::metadata(e.path()) else { continue };
                if meta.is_dir() {
                    if rel.is_empty() && matches!(name.as_str(), "posix" | "right") {
                        continue;
                    }
                    stack.push(path);
                } else if name != "localtime" && name != "posixrules" && is_tzif(&e.path()) {
                    self.names.insert(path.to_ascii_lowercase(), path);
                }
            }
        }
        // The links (`L target name`), which a system may not have files of.
        if let Ok(text) = std::fs::read_to_string(format!("{dir}/tzdata.zi")) {
            for line in text.lines() {
                let mut w = line.split_whitespace();
                if w.next() == Some("L") {
                    if let (Some(target), Some(name)) = (w.next(), w.next()) {
                        self.names.entry(name.to_ascii_lowercase()).or_insert_with(|| name.to_string());
                        self.links.insert(name.to_string(), target.to_string());
                    }
                }
            }
        }
    }

    /// The primary identifier of available name `name`: links followed.
    fn primary(&self, name: &str) -> String {
        let mut n = name.to_string();
        for _ in 0..8 {
            if let Some(t) = self.links.get(&n) {
                n = t.clone();
                continue;
            }
            let path = format!("{}/{n}", self.dir);
            match std::fs::read_link(&path) {
                Ok(target) => {
                    let base = std::path::Path::new(&n).parent().map(|p| p.to_path_buf()).unwrap_or_default();
                    let joined = base.join(target);
                    // (Normalized: `..` components.)
                    let mut parts: Vec<String> = Vec::new();
                    for c in joined.components() {
                        match c {
                            std::path::Component::ParentDir => {
                                parts.pop();
                            }
                            std::path::Component::Normal(s) => parts.push(s.to_string_lossy().into_owned()),
                            _ => {}
                        }
                    }
                    n = parts.join("/");
                }
                Err(_) => break,
            }
        }
        n
    }

    /// GetAvailableNamedTimeZoneIdentifier: the zone named `name` (any
    /// case), if there is one.
    pub fn find(&mut self, name: &str) -> Option<Zone> {
        self.scan();
        let lower = name.to_ascii_lowercase();
        let id = if lower == "utc" { "UTC".to_string() } else { self.names.get(&lower)?.clone() };
        let primary = self.primary(&id);
        let data = match self.loaded.get(&primary) {
            Some(d) => d.clone(),
            None => {
                let bytes = std::fs::read(format!("{}/{primary}", self.dir)).ok();
                let d = match bytes.and_then(|b| parse_tzif(&b)) {
                    Some(d) => Rc::new(d),
                    None if lower == "utc" => Rc::new(Data { initial: 0, transitions: Vec::new(), rule: None }),
                    None => return None,
                };
                self.loaded.insert(primary.clone(), d.clone());
                d
            }
        };
        Some(Zone { id, kind: ZoneKind::Named(primary, data) })
    }
}

fn is_tzif(path: &std::path::Path) -> bool {
    use std::io::Read;
    let mut magic = [0u8; 4];
    std::fs::File::open(path).and_then(|mut f| f.read_exact(&mut magic)).is_ok() && &magic == b"TZif"
}

/// The system's time zone: `TZ` if it names one, else `/etc/localtime`'s
/// link, else UTC.
pub fn system_zone_name() -> String {
    if let Ok(tz) = std::env::var("TZ") {
        let tz = tz.trim_start_matches(':').to_string();
        if !tz.is_empty() && !tz.starts_with('/') {
            return tz;
        }
    }
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let s = target.to_string_lossy().into_owned();
        if let Some(k) = s.find("zoneinfo/") {
            return s[k + 9..].to_string();
        }
    }
    "UTC".into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn posix_rules() {
        let r = parse_posix("CET-1CEST,M3.5.0,M10.5.0/3").unwrap();
        assert_eq!(r.std, 3600);
        // 2021-03-28T01:00Z: summer time.
        let t = iso::epoch_days(2021, 3, 28) * 86400 + 3600;
        assert_eq!(r.offset_at(t - 1), 3600);
        assert_eq!(r.offset_at(t), 7200);
        let r = parse_posix("<-03>3").unwrap();
        assert_eq!(r.std, -10800);
    }

    #[test]
    fn named_zones() {
        let mut reg = Registry::default();
        let Some(z) = reg.find("europe/vienna") else { return };
        assert_eq!(z.id, "Europe/Vienna");
        let ns = iso::utc_epoch_ns(DateTime { date: iso::IsoDate::new(2021, 7, 1), time: iso::Time::MIDNIGHT });
        assert_eq!(z.offset_ns(ns), 7_200_000_000_000);
        let next = z.next_transition(ns).unwrap();
        assert_eq!(iso::from_epoch_ns(next).date, iso::IsoDate::new(2021, 10, 31));
        let prev = z.previous_transition(ns).unwrap();
        assert_eq!(iso::from_epoch_ns(prev).date, iso::IsoDate::new(2021, 3, 28));
        let far = iso::utc_epoch_ns(DateTime { date: iso::IsoDate::new(2200, 7, 1), time: iso::Time::MIDNIGHT });
        assert_eq!(iso::from_epoch_ns(z.next_transition(far).unwrap()).date, iso::IsoDate::new(2200, 10, 26));
        assert_eq!(iso::from_epoch_ns(z.previous_transition(far).unwrap()).date, iso::IsoDate::new(2200, 3, 30));
    }
}
