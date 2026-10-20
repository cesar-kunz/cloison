//! Regular expressions: a parser for ECMAScript's pattern syntax, compiled
//! to instructions ([`Insn`]) that a backtracking interpreter runs over
//! UTF-16 code units (as the strings are).
//!
//! The interpreter keeps its choice points on a stack of its own, with
//! every change to a capture or a register logged there, so backtracking
//! undoes it: a failed match changes no captures, and nothing recurses per
//! character. The tests keep the earlier matcher, which passes
//! continuations over the pattern's tree, as the reference.

/// A compiled regular expression.
pub struct Regex {
    node: Node,
    /// `node` compiled.
    prog: Program,
    /// The interpreter's state, kept from one `exec` to the next (its
    /// stack grown once).
    run: std::cell::RefCell<Run>,
    /// Capture groups, group 0 (the whole match) included.
    pub groups: usize,
    /// The named groups (`(?<name>…)`), with their indices.
    pub names: Vec<(String, usize)>,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    /// `y`: a match only where the search starts (at `lastIndex`).
    pub sticky: bool,
    /// `u`: code points (a surrogate pair is one character).
    pub unicode: bool,
    /// `s`: `.` matches line terminators too.
    pub dot_all: bool,
    /// Where a match can start (see [`Starts`]).
    starts: Starts,
    /// A literal every match contains, if any (see [`Needle`]).
    needle: Option<Needle>,
    /// The pattern, if it is just characters (case-sensitive): found by
    /// searching for them, without the interpreter.
    literal: Option<Vec<u16>>,
    /// `prog` as native code, compiled at the `native_after()`th exec
    /// (`None` inside: it has instructions that don't compile).
    native: std::cell::OnceCell<Option<Native>>,
    execs: std::cell::Cell<u32>,
}

/// A run of characters in the pattern's top-level sequence, so in every
/// match: `exec` tries only where one follows within what the part before
/// it can match.
struct Needle {
    units: Vec<u16>,
    /// The shortest and longest the part before it matches (`None`: no
    /// bound).
    min: usize,
    max: Option<usize>,
}

/// Where a match can start, so `exec` tries only there.
#[derive(Clone, Copy)]
enum Starts {
    /// Anywhere: the pattern may match empty, or begin with a backreference.
    Anywhere,
    /// Where `^` matches (every alternative begins with it).
    Anchored,
    /// At a code unit in `ascii` (bit `c` for `c` < 128), or at any one
    /// past ASCII if `other`; `few` if those are at most four units.
    Units { ascii: u128, other: bool, few: Option<([u16; 4], usize)> },
}

/// The code units a node's non-empty matches can begin with (as in
/// `Starts::Units`), and whether it can match empty (then what follows it
/// can begin a match too); `None` if unknown.
type First = Option<(u128, bool, bool)>;

/// Where each group matched (start, end), if it did.
pub type Captures = Vec<Option<(usize, usize)>>;

#[derive(Debug)]
enum Node {
    Empty,
    Char(u16),
    /// `.`: anything but a line terminator.
    Any,
    /// A class: sorted inclusive ranges, and whether it is negated.
    Class(Vec<(u16, u16)>, bool),
    /// A surrogate pair whose astral code point is in the sorted inclusive
    /// ranges (`u` mode).
    Astral(Vec<(u32, u32)>),
    /// `^`, `$`.
    Start,
    End,
    /// `\b` (true) or `\B`.
    Boundary(bool),
    /// A group: capturing (its index) or not.
    Group(Box<Node>, Option<usize>),
    /// `(?=…)` (true) or `(?!…)`; behind (the third), `(?<=…)` or
    /// `(?<!…)`.
    Look(Box<Node>, bool, bool),
    BackRef(usize),
    Alt(Vec<Node>),
    Seq(Vec<Node>),
    /// A repetition: minimum, maximum (`u32::MAX`: none), greedy, and the
    /// capture groups inside it (cleared at each iteration).
    Repeat(Box<Node>, u32, u32, bool, std::ops::Range<usize>),
}

/// `u` mode's lone surrogates in `n`, where the input is code points: one,
/// as a character or a class's member, matches only alone, not half of a
/// pair (a lead not followed by a trail, a trail not preceded by a lead).
/// (`.` and negated classes match a pair whole already: see `atom`.)
fn code_points(n: Node) -> Node {
    let rec = |b: Box<Node>| Box::new(code_points(*b));
    match n {
        Node::Char(c @ 0xD800..=0xDBFF) => Node::Seq(vec![Node::Char(c), not_followed_by_trail()]),
        Node::Char(c @ 0xDC00..=0xDFFF) => Node::Seq(vec![not_preceded_by_lead(), Node::Char(c)]),
        Node::Class(r, false) if r.iter().any(|&(a, b)| a <= TRAILS.1 && b >= LEADS.0) => unit_class(&r),
        Node::Group(b, g) => Node::Group(rec(b), g),
        Node::Look(b, pos, behind) => Node::Look(rec(b), pos, behind),
        Node::Alt(v) => Node::Alt(v.into_iter().map(code_points).collect()),
        // (A lead then a trail, `\ud83d\ude00`, a surrogate pair's classes,
        // is a pair's code point.)
        Node::Seq(v) => {
            let mut out = Vec::with_capacity(v.len());
            let mut it = v.into_iter().peekable();
            while let Some(n) = it.next() {
                let lead = |n: &Node| match n {
                    Node::Char(c) => (LEADS.0..=LEADS.1).contains(c),
                    Node::Class(r, false) => r.iter().all(|&(a, b)| a >= LEADS.0 && b <= LEADS.1),
                    _ => false,
                };
                let trail = |n: &Node| match n {
                    Node::Char(c) => (TRAILS.0..=TRAILS.1).contains(c),
                    Node::Class(r, false) => r.iter().all(|&(a, b)| a >= TRAILS.0 && b <= TRAILS.1),
                    _ => false,
                };
                if lead(&n) && it.peek().is_some_and(trail) {
                    out.push(n);
                    out.push(it.next().unwrap());
                    continue;
                }
                out.push(code_points(n));
            }
            Node::Seq(out)
        }
        Node::Repeat(b, min, max, greedy, gs) => Node::Repeat(rec(b), min, max, greedy, gs),
        n => n,
    }
}

const LEADS: (u16, u16) = (0xD800, 0xDBFF);
const TRAILS: (u16, u16) = (0xDC00, 0xDFFF);

fn not_followed_by_trail() -> Node {
    Node::Look(Box::new(Node::Class(vec![TRAILS], false)), false, false)
}

fn not_preceded_by_lead() -> Node {
    Node::Look(Box::new(Node::Class(vec![LEADS], false)), false, true)
}

/// A class of the units in `r` in `u` mode: its surrogates alone only (see
/// [`code_points`]).
fn unit_class(r: &[(u16, u16)]) -> Node {
    let part = |(lo, hi): (u16, u16)| -> Vec<(u16, u16)> {
        r.iter().filter_map(|&(a, b)| (a.max(lo) <= b.min(hi)).then(|| (a.max(lo), b.min(hi)))).collect()
    };
    let (leads, trails) = (part(LEADS), part(TRAILS));
    let others: Vec<(u16, u16)> = r
        .iter()
        .flat_map(|&(a, b)| [(a, b.min(LEADS.0 - 1)), (a.max(TRAILS.1 + 1), b)])
        .filter(|&(x, y)| x <= y && (y < LEADS.0 || x > TRAILS.1))
        .collect();
    let mut alts = Vec::new();
    if !others.is_empty() {
        alts.push(Node::Class(others, false));
    }
    if !leads.is_empty() {
        alts.push(Node::Seq(vec![Node::Class(leads, false), not_followed_by_trail()]));
    }
    if !trails.is_empty() {
        alts.push(Node::Seq(vec![not_preceded_by_lead(), Node::Class(trails, false)]));
    }
    Node::Alt(alts)
}

impl Regex {
    /// Whether `pattern` with `flags` is a valid regular expression (a
    /// literal's early error), as [`Regex::new`] would find, without
    /// compiling it.
    pub fn check(pattern: &str, flags: &str) -> Result<(), String> {
        Regex::parse(pattern, flags).map(|_| ())
    }

    /// `pattern`'s tree, group count and names, and `flags`
    /// (`g`, `i`, `m`, `y`, `u`, `s`, in that order).
    #[allow(clippy::type_complexity)]
    fn parse(pattern: &str, flags: &str) -> Result<(Node, usize, Vec<(String, usize)>, [bool; 6]), String> {
        let (mut global, mut ignore_case, mut multiline) = (false, false, false);
        let (mut sticky, mut unicode, mut dot_all) = (false, false, false);
        for f in flags.chars() {
            let seen = match f {
                'g' => std::mem::replace(&mut global, true),
                'i' => std::mem::replace(&mut ignore_case, true),
                'm' => std::mem::replace(&mut multiline, true),
                'y' => std::mem::replace(&mut sticky, true),
                'u' => std::mem::replace(&mut unicode, true),
                's' => std::mem::replace(&mut dot_all, true),
                _ => return Err(format!("Invalid flags supplied to RegExp constructor '{flags}'")),
            };
            if seen {
                return Err(format!("Invalid flags supplied to RegExp constructor '{flags}'"));
            }
        }
        let chars: Vec<char> = pattern.chars().collect();
        let (total, names) = count_groups(&chars);
        let mut p = Parser {
            c: &chars,
            i: 0,
            groups: 1,
            total,
            names,
            unicode,
            dot_all,
            depth: 0,
            pending: None,
        };
        let node = p.alternation()?;
        if p.i != chars.len() {
            return Err(format!("Invalid regular expression: /{pattern}/: Unmatched ')'"));
        }
        Ok((node, p.groups, p.names, [global, ignore_case, multiline, sticky, unicode, dot_all]))
    }

    /// Compile `pattern` with `flags` (`g`, `i`, `m`, `y`, `u`, `s`).
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        let (node, groups, names, [global, ignore_case, multiline, sticky, unicode, dot_all]) = Regex::parse(pattern, flags)?;
        let node = if unicode { code_points(node) } else { node };
        let mut re = Regex {
            node,
            groups,
            names,
            global,
            ignore_case,
            multiline,
            sticky,
            unicode,
            dot_all,
            starts: Starts::Anywhere,
            needle: None,
            prog: Program::default(),
            run: Default::default(),
            literal: None,
            native: Default::default(),
            execs: Default::default(),
        };
        re.prog = Program::compile(&re);
        re.literal = match &re.node {
            _ if re.ignore_case => None,
            Node::Char(c) => Some(vec![*c]),
            Node::Seq(ns) => ns
                .iter()
                .map(|n| match n {
                    Node::Char(c) => Some(*c),
                    _ => None,
                })
                .collect(),
            _ => None,
        };
        re.starts = re.starts();
        re.needle = re.needle();
        Ok(re)
    }

    /// The longest run of characters in the top-level sequence that match
    /// only themselves (folding case, ASCII but letters): of two or more,
    /// or of one when finding where a match can start is slow otherwise.
    fn needle(&self) -> Option<Needle> {
        let Node::Seq(ns) = &self.node else {
            return None;
        };
        // Folding case, characters that only match themselves: ASCII but
        // letters.
        let ic = self.ignore_case;
        let fixed = |n: &Node| matches!(n, Node::Char(c) if !ic || *c < 128 && !(*c as u8).is_ascii_alphabetic());
        // One unit will do where finding a start is slow otherwise.
        let one = if matches!(self.starts, Starts::Units { few: Some(_), .. }) { 2 } else { 1 };
        let mut best: Option<(usize, usize)> = None;
        let mut k = 0;
        while k < ns.len() {
            let run = ns[k..].iter().take_while(|n| fixed(n)).count();
            if run >= one && best.is_none_or(|(_, len)| run > len) {
                best = Some((k, run));
            }
            k += run.max(1);
        }
        let (k, len) = best?;
        let units = ns[k..k + len]
            .iter()
            .map(|n| match n {
                Node::Char(c) => *c,
                _ => unreachable!(),
            })
            .collect();
        let (min, max) = seq_width(&ns[..k]);
        Some(Needle { units, min, max })
    }

    fn starts(&self) -> Starts {
        if anchored(&self.node) {
            return Starts::Anchored;
        }
        match self.first(&self.node) {
            Some((ascii, other, false)) => Starts::Units {
                ascii,
                other,
                few: (ascii.count_ones() <= 4 && !other).then(|| {
                    let mut u = [0; 4];
                    let (mut n, mut m) = (0, ascii);
                    while m != 0 {
                        u[n] = m.trailing_zeros() as u16;
                        m &= m - 1;
                        n += 1;
                    }
                    (u, n)
                }),
            },
            _ => Starts::Anywhere,
        }
    }

    /// The first match in `s` at or after `start`: each group's span.
    pub fn exec(&self, s: &[u16], start: usize) -> Option<Captures> {
        let mut caps = Vec::new();
        self.exec_into(s, start, &mut caps).then_some(caps)
    }

    /// [`Regex::exec`] into `caps` (no allocation once it has room):
    /// whether it matched.
    pub fn exec_into(&self, s: &[u16], start: usize, caps: &mut Captures) -> bool {
        caps.clear();
        if let Some(l) = &self.literal {
            if self.sticky {
                if s.get(start..start + l.len()) != Some(&l[..]) {
                    return false;
                }
                caps.push(Some((start, start + l.len())));
                return true;
            }
            let Some(q) = (start <= s.len()).then(|| find(s, l, start)).flatten() else {
                return false;
            };
            caps.push(Some((q, q + l.len())));
            return true;
        }
        if self.native.get().is_none() {
            let n = self.execs.get() + 1;
            self.execs.set(n);
            if n >= native_after() {
                let _ = self.native.set(Native::compile(self));
            }
        }
        let native = self.native.get().and_then(Option::as_ref);
        let mut kept = self.run.try_borrow_mut().ok();
        let mut fresh;
        let run = match kept.as_deref_mut() {
            Some(run) => run,
            None => {
                fresh = Run::default();
                &mut fresh
            }
        };
        run.caps.resize(self.groups, None);
        run.regs.resize(self.prog.nregs, 0);
        // Native code's registers, clean from one attempt to the next (a
        // failed one undoes its changes) unless one bailed.
        let mut clean = false;
        let mut attempt = |at, run: &mut Run| {
            if let Some(n) = native {
                if !clean {
                    n.begin(run);
                    clean = true;
                }
                match n.run(self, s, at, run) {
                    Some(matched) => return matched,
                    None => clean = false,
                }
            }
            self.prog.run(self, s, at, run)
        };
        if self.search(s, start, |at| attempt(at, run)).is_none() {
            return false;
        }
        caps.extend_from_slice(&run.caps);
        true
    }

    /// Call `attempt` at each position from `start` where a match can
    /// begin, until it finds one: where.
    fn search(&self, s: &[u16], start: usize, mut attempt: impl FnMut(usize) -> bool) -> Option<usize> {
        if self.sticky {
            return (start <= s.len() && attempt(start)).then_some(start);
        }
        let mut at = start;
        // Where the needle next occurs, at or after `at` + its `min`.
        let mut next: Option<usize> = None;
        while at <= s.len() {
            if let Some(nd) = &self.needle {
                let from = at.saturating_add(nd.min);
                if next.is_none_or(|q| q < from) {
                    next = Some(find(s, &nd.units, from)?);
                }
                // A match from before `q` - `max` would need the needle
                // before `q`.
                if let (Some(q), Some(max)) = (next, nd.max) {
                    at = at.max(q.saturating_sub(max));
                }
            }
            // Skip to where a match can start.
            match self.starts {
                Starts::Anywhere => {}
                Starts::Anchored => {
                    let line = |i: usize| i == 0 || self.multiline && is_line_terminator(s[i - 1]);
                    if !line(at) {
                        if !self.multiline {
                            return None;
                        }
                        at += s[at..].iter().position(|&c| is_line_terminator(c))? + 1;
                    }
                }
                Starts::Units { few: Some((u, n)), .. } => {
                    at = find_any(s, &u[..n], at)?;
                }
                Starts::Units { ascii, other, .. } => {
                    let can = |c: u16| if c < 128 { ascii >> c & 1 != 0 } else { other };
                    at += s[at..].iter().position(|&c| can(c))?;
                }
            }
            if attempt(at) {
                return Some(at);
            }
            at += 1;
        }
        None
    }

    /// What `n`'s matches can begin with (see [`First`]): ASCII units are
    /// tested as matching does, so with its case folding (which maps no
    /// unit past ASCII to one in it, or back).
    fn first(&self, n: &Node) -> First {
        let ic = Case::of(self);
        let units = |one: &dyn Fn(u16) -> bool, other: bool| {
            let ascii = (0..128u16).filter(|&c| one(c)).fold(0u128, |m, c| m | 1 << c);
            Some((ascii, other, false))
        };
        match n {
            Node::Empty | Node::Start | Node::End | Node::Boundary(_) | Node::Look(..) => {
                Some((0, false, true))
            }
            // (Case folding maps no unit past ASCII to one in it, or back,
            // but Unicode's: `ſ` and the Kelvin sign.)
            Node::Char(c) => units(&|x| eq(ic, x, *c), *c >= 128 || ic == Case::Unicode),
            Node::Any => units(&|x| !is_line_terminator(x), true),
            Node::Class(r, neg) => units(
                &|x| in_class(ic, r, *neg, x),
                *neg || r.iter().any(|&(_, hi)| hi >= 128) || ic == Case::Unicode,
            ),
            Node::Astral(_) => Some((0, true, false)),
            Node::Group(inner, _) => self.first(inner),
            Node::BackRef(_) => None,
            Node::Repeat(inner, min, ..) => {
                let (a, o, empty) = self.first(inner)?;
                Some((a, o, empty || *min == 0))
            }
            Node::Alt(alts) => alts.iter().try_fold((0, false, false), |(a, o, e), n| {
                let (a2, o2, e2) = self.first(n)?;
                Some((a | a2, o || o2, e || e2))
            }),
            Node::Seq(nodes) => {
                let (mut a, mut o) = (0, false);
                for n in nodes {
                    let (a2, o2, empty) = self.first(n)?;
                    (a, o) = (a | a2, o || o2);
                    if !empty {
                        return Some((a, o, false));
                    }
                }
                Some((a, o, true))
            }
        }
    }
}

/// Whether `a` matches `b`, folding case if `ic`.
fn eq(ic: Case, a: u16, b: u16) -> bool {
    a == b || (ic != Case::Exact && ic.canon(a) == ic.canon(b))
}

/// How case compares: exactly, folded as without `u` (`i`), or by Unicode
/// simple case folding (`i` and `u`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Case {
    #[default]
    Exact,
    Fold,
    Unicode,
}

impl Case {
    fn of(re: &Regex) -> Case {
        match (re.ignore_case, re.unicode) {
            (false, _) => Case::Exact,
            (true, false) => Case::Fold,
            (true, true) => Case::Unicode,
        }
    }

    /// `c`'s form, the same for units that match each other.
    fn canon(self, c: u16) -> u16 {
        match self {
            Case::Exact => c,
            Case::Fold => canonical(c),
            Case::Unicode => simple_fold(c),
        }
    }

    /// Whether `c` is a word character (`\b`): with `u` and `i`, `ſ` and
    /// the Kelvin sign too, which fold to `s` and `k`.
    fn word(self, c: u16) -> bool {
        is_word(c) || self == Case::Unicode && matches!(c, 0x017F | 0x212A)
    }
}

/// `c`'s simple case folding (CaseFolding.txt's C and S, a unit's): its
/// upper case's lower case, where each is one character (the same for the
/// units that fold together); `ı` folds to itself.
fn simple_fold(c: u16) -> u16 {
    let Some(ch) = char::from_u32(c as u32) else {
        return c;
    };
    if c < 0x80 {
        return (c as u8).to_ascii_lowercase() as u16;
    }
    if c == 0x0131 {
        return c;
    }
    let one = |s: &mut dyn Iterator<Item = char>| match (s.next(), s.next()) {
        (Some(x), None) => Some(x),
        _ => None,
    };
    let up = one(&mut ch.to_uppercase()).unwrap_or(ch);
    let low = one(&mut up.to_lowercase()).unwrap_or(up);
    if (low as u32) < 0x10000 { low as u16 } else { c }
}

/// Whether `c` is in the class of `ranges` (negated if `neg`), folding
/// case if `ic`.
fn in_class(ic: Case, ranges: &[(u16, u16)], neg: bool, c: u16) -> bool {
    let has = |c: u16| ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
    let mut r = has(c);
    if !r && ic != Case::Exact {
        let u = ic.canon(c);
        r = has(u)
            || ranges
                .iter()
                .any(|&(lo, hi)| (lo..=hi).len() < 512 && (lo..=hi).any(|x| ic.canon(x) == u));
    }
    r != neg
}

/// The first of `units` (one to four) in `s` at or after `from`.
fn find_any(s: &[u16], units: &[u16], from: usize) -> Option<usize> {
    #[cfg_attr(not(target_arch = "x86_64"), allow(unused_mut))]
    let mut at = from;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use core::arch::x86_64::*;
        let k = |i: usize| _mm_set1_epi16(units[i.min(units.len() - 1)] as i16);
        let (u0, u1, u2, u3) = (k(0), k(1), k(2), k(3));
        let p = s.as_ptr();
        while at + 8 <= s.len() {
            let a = _mm_loadu_si128(p.add(at).cast());
            let eq = _mm_or_si128(
                _mm_or_si128(_mm_cmpeq_epi16(a, u0), _mm_cmpeq_epi16(a, u1)),
                _mm_or_si128(_mm_cmpeq_epi16(a, u2), _mm_cmpeq_epi16(a, u3)),
            );
            let m = _mm_movemask_epi8(eq) as u32;
            if m != 0 {
                return Some(at + (m.trailing_zeros() / 2) as usize);
            }
            at += 8;
        }
    }
    (at..s.len()).find(|&i| units.contains(&s[i]))
}

/// The first `needle` in `s` at or after `from`: candidates where its
/// first and last units both occur, eight positions at a time.
pub(crate) fn find(s: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    let n = needle.len();
    if n == 0 {
        return (from <= s.len()).then_some(from);
    }
    if from + n > s.len() {
        return None;
    }
    let last = s.len() - n;
    let mut at = from;
    #[cfg(target_arch = "x86_64")]
    unsafe {
        use core::arch::x86_64::*;
        let f = _mm_set1_epi16(needle[0] as i16);
        let l = _mm_set1_epi16(needle[n - 1] as i16);
        let p = s.as_ptr();
        while at + 8 <= last + 1 {
            let a = _mm_loadu_si128(p.add(at).cast());
            let b = _mm_loadu_si128(p.add(at + n - 1).cast());
            let eq = _mm_and_si128(_mm_cmpeq_epi16(a, f), _mm_cmpeq_epi16(b, l));
            let mut m = _mm_movemask_epi8(eq) as u32 & 0x5555;
            while m != 0 {
                let i = at + (m.trailing_zeros() / 2) as usize;
                if n <= 2 || s[i + 1..i + n - 1] == needle[1..n - 1] {
                    return Some(i);
                }
                m &= m - 1;
            }
            at += 8;
        }
    }
    while at <= last {
        if s[at] == needle[0] && s[at..at + n] == *needle {
            return Some(at);
        }
        at += 1;
    }
    None
}

/// The shortest and longest `n` matches (`None`: no bound).
fn width(n: &Node) -> (usize, Option<usize>) {
    match n {
        Node::Empty | Node::Start | Node::End | Node::Boundary(_) | Node::Look(..) => (0, Some(0)),
        Node::Char(_) | Node::Any | Node::Class(..) => (1, Some(1)),
        Node::Astral(_) => (2, Some(2)),
        Node::Group(inner, _) => width(inner),
        Node::BackRef(_) => (0, None),
        Node::Alt(alts) => alts.iter().map(width).fold((usize::MAX, Some(0)), |(a, b), (c, d)| {
            (a.min(c), b.zip(d).map(|(b, d)| b.max(d)))
        }),
        Node::Seq(nodes) => seq_width(nodes),
        Node::Repeat(inner, min, max, ..) => {
            let (c, d) = width(inner);
            let hi = match (d, *max) {
                (Some(0), _) => Some(0),
                (_, u32::MAX) => None,
                (d, max) => d.map(|d| d.saturating_mul(max as usize)),
            };
            (c.saturating_mul(*min as usize), hi)
        }
    }
}

fn seq_width(nodes: &[Node]) -> (usize, Option<usize>) {
    nodes.iter().map(width).fold((0, Some(0)), |(a, b), (c, d)| {
        (a.saturating_add(c), b.zip(d).map(|(b, d)| b.saturating_add(d)))
    })
}

/// Whether every match of `n` begins where `^` matches.
fn anchored(n: &Node) -> bool {
    match n {
        Node::Start => true,
        Node::Group(inner, _) => anchored(inner),
        Node::Seq(nodes) => nodes.first().is_some_and(anchored),
        Node::Alt(alts) => alts.iter().all(anchored),
        _ => false,
    }
}

/// The number of capturing groups (for telling `\N` backreferences).
/// The capture groups in pattern `c`, and the named ones (with their
/// indices).
fn count_groups(c: &[char]) -> (usize, Vec<(String, usize)>) {
    let (mut n, mut i, mut class) = (0, 0, false);
    let mut names = Vec::new();
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '[' => class = true,
            ']' => class = false,
            '(' if !class && c.get(i + 1) != Some(&'?') => n += 1,
            // `(?<name>`, not `(?<=` or `(?<!`.
            '(' if !class && c[i + 1..].starts_with(&['?', '<']) && !matches!(c.get(i + 3), Some('=' | '!')) => {
                n += 1;
                let name: String = c[i + 3..].iter().take_while(|&&ch| ch != '>').collect();
                names.push((name, n));
            }
            _ => {}
        }
        i += 1;
    }
    (n, names)
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
    /// The next capturing group's index.
    groups: usize,
    total: usize,
    /// The named groups (all of them, found first).
    names: Vec<(String, usize)>,
    unicode: bool,
    dot_all: bool,
    /// Groups open around the current position (up to [`MAX_NESTING`]).
    depth: usize,
    /// An error a class's member found (see `Parser::class_atom`).
    pending: Option<String>,
}

/// The deepest groups may nest: the parser, and the passes over the tree
/// after it, recurse once per level, on the native stack.
const MAX_NESTING: usize = 500;

impl Parser<'_> {
    fn peek(&self) -> Option<char> {
        self.c.get(self.i).copied()
    }

    fn error(&self, m: &str) -> String {
        let src: String = self.c.iter().collect();
        format!("Invalid regular expression: /{src}/: {m}")
    }

    fn alternation(&mut self) -> Result<Node, String> {
        let mut alts = vec![self.sequence()?];
        while self.peek() == Some('|') {
            self.i += 1;
            alts.push(self.sequence()?);
        }
        Ok(if alts.len() == 1 { alts.pop().unwrap() } else { Node::Alt(alts) })
    }

    fn sequence(&mut self) -> Result<Node, String> {
        let mut seq = Vec::new();
        while let Some(c) = self.peek() {
            if c == '|' || c == ')' {
                break;
            }
            let first = self.groups;
            let atom = self.atom()?;
            let atom = self.quantifier(atom, first..self.groups)?;
            seq.push(atom);
        }
        Ok(match seq.len() {
            0 => Node::Empty,
            1 => seq.pop().unwrap(),
            _ => Node::Seq(seq),
        })
    }

    fn quantifier(&mut self, atom: Node, groups: std::ops::Range<usize>) -> Result<Node, String> {
        let (min, max) = match self.peek() {
            Some('*') => (0, u32::MAX),
            Some('+') => (1, u32::MAX),
            Some('?') => (0, 1),
            // (`braces` consumes it.)
            Some('{') => match self.braces() {
                Some(r) => {
                    self.i -= 1;
                    r
                }
                None => return Ok(atom),
            },
            _ => return Ok(atom),
        };
        self.i += 1;
        if min > max {
            return Err(self.error("numbers out of order in {} quantifier"));
        }
        // (A lookahead may be, outside `u` mode: Annex B.)
        if matches!(atom, Node::Start | Node::End | Node::Boundary(_))
            || matches!(atom, Node::Look(..)) && self.unicode
        {
            return Err(self.error("Nothing to repeat"));
        }
        let greedy = if self.peek() == Some('?') {
            self.i += 1;
            false
        } else {
            true
        };
        Ok(Node::Repeat(Box::new(atom), min, max, greedy, groups))
    }

    /// `{n}`, `{n,}`, `{n,m}` at `{` (consumed if it is one; else `{` is
    /// a literal).
    fn braces(&mut self) -> Option<(u32, u32)> {
        let start = self.i;
        let num = |p: &mut Self| {
            let s = p.i;
            while p.peek().is_some_and(|c| c.is_ascii_digit()) {
                p.i += 1;
            }
            (p.i > s).then(|| p.c[s..p.i].iter().collect::<String>().parse::<u32>().unwrap_or(u32::MAX))
        };
        self.i += 1;
        let r = (|| {
            let min = num(self)?;
            let max = if self.peek() == Some(',') {
                self.i += 1;
                num(self).unwrap_or(u32::MAX)
            } else {
                min
            };
            (self.peek() == Some('}')).then(|| {
                self.i += 1;
                (min, max)
            })
        })();
        if r.is_none() {
            self.i = start;
        }
        r
    }

    fn atom(&mut self) -> Result<Node, String> {
        let c = self.peek().unwrap();
        self.i += 1;
        Ok(match c {
            '.' if self.dot_all => {
                let any = Node::Class(vec![(0, 0xFFFF)], false);
                if self.unicode { Node::Alt(vec![surrogate_pair(), any]) } else { any }
            }
            '.' if self.unicode => Node::Alt(vec![surrogate_pair(), Node::Any]),
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                let (kind, cap) = if self.c[self.i..].starts_with(&['?', ':']) {
                    self.i += 2;
                    (0, None)
                } else if self.c[self.i..].starts_with(&['?', '=']) {
                    self.i += 2;
                    (1, None)
                } else if self.c[self.i..].starts_with(&['?', '!']) {
                    self.i += 2;
                    (2, None)
                } else if self.c[self.i..].starts_with(&['?', '<', '=']) {
                    self.i += 3;
                    (3, None)
                } else if self.c[self.i..].starts_with(&['?', '<', '!']) {
                    self.i += 3;
                    (4, None)
                } else if self.c[self.i..].starts_with(&['?', '<']) {
                    // `(?<name>…)`
                    self.i += 2;
                    let name = self.group_name()?;
                    let g = self.groups;
                    self.groups += 1;
                    if self.names.iter().filter(|(n, _)| *n == name).count() > 1 {
                        return Err(self.error("Duplicate capture group name"));
                    }
                    (0, Some(g))
                } else {
                    let g = self.groups;
                    self.groups += 1;
                    (0, Some(g))
                };
                self.depth += 1;
                if self.depth > MAX_NESTING {
                    return Err(self.error("Regular expression too large"));
                }
                let inner = self.alternation()?;
                self.depth -= 1;
                if self.peek() != Some(')') {
                    return Err(self.error("Unterminated group"));
                }
                self.i += 1;
                match kind {
                    0 => Node::Group(Box::new(inner), cap),
                    k => Node::Look(Box::new(inner), k == 1 || k == 3, k >= 3),
                }
            }
            ')' => return Err(self.error("Unmatched ')'")),
            '[' => self.class()?,
            '\\' => self.escape()?,
            '{' if self.braces_at_atom() => return Err(self.error("Nothing to repeat")),
            '{' | '}' | ']' if self.unicode => return Err(self.error("Lone quantifier brackets")),
            '*' | '+' | '?' => return Err(self.error("Nothing to repeat")),
            c => char_node(c),
        })
    }

    /// Whether a `{` just consumed as an atom is a quantifier (an error).
    fn braces_at_atom(&mut self) -> bool {
        self.i -= 1;
        let q = self.braces().is_some();
        if !q {
            self.i += 1;
        }
        q
    }

    /// A group's name, past its `<`, and its `>`.
    fn group_name(&mut self) -> Result<String, String> {
        let s = self.i;
        while self.peek().is_some_and(|c| c != '>') {
            self.i += 1;
        }
        let name: String = self.c[s..self.i].iter().collect();
        let ok = name.chars().next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
            && name.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '$');
        if self.peek() != Some('>') || !ok {
            return Err(self.error("Invalid capture group name"));
        }
        self.i += 1;
        Ok(name)
    }

    fn escape(&mut self) -> Result<Node, String> {
        let Some(e) = self.peek() else {
            return Err(self.error("\\ at end of pattern"));
        };
        self.i += 1;
        Ok(match e {
            // `\k<name>`: a named group's backreference (else, without
            // named groups and `u`, a `k`).
            'k' if !self.names.is_empty() || self.unicode => {
                if self.peek() != Some('<') {
                    return Err(self.error("Invalid named reference"));
                }
                self.i += 1;
                let name = self.group_name()?;
                match self.names.iter().find(|(n, _)| *n == name) {
                    Some(&(_, g)) => Node::BackRef(g),
                    None => return Err(self.error("Invalid named capture referenced")),
                }
            }
            'b' => Node::Boundary(true),
            'B' => Node::Boundary(false),
            'p' | 'P' if self.unicode => {
                let set = self.property(e == 'P')?;
                cp_class(set, false)
            }
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                let (ranges, neg) = class_escape(e);
                if neg && self.unicode {
                    Node::Alt(vec![surrogate_pair(), Node::Class(ranges, neg)])
                } else {
                    Node::Class(ranges, neg)
                }
            }
            'u' if self.unicode && self.peek() == Some('{') => {
                // `\u{…}`: a code point.
                let s = self.i + 1;
                let Some(end) = self.c[s..].iter().position(|&c| c == '}') else {
                    return Err(self.error("Invalid Unicode escape"));
                };
                let hex: String = self.c[s..s + end].iter().collect();
                let cp = u32::from_str_radix(&hex, 16).ok().and_then(char::from_u32);
                let Some(c) = cp.filter(|_| !hex.is_empty()) else {
                    return Err(self.error("Invalid Unicode escape"));
                };
                self.i = s + end + 1;
                char_node(c)
            }
            '1'..='9' => {
                let s = self.i - 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
                let n: usize = self.c[s..self.i].iter().collect::<String>().parse().unwrap_or(0);
                if n <= self.total {
                    Node::BackRef(n)
                } else if self.unicode {
                    return Err(self.error("Invalid escape"));
                } else {
                    // Not a group: an octal escape, or the digits.
                    self.i = s;
                    let mut v = 0u32;
                    while let Some(d) = self.peek().and_then(|c| c.to_digit(8)) {
                        if v * 8 + d > 0o377 {
                            break;
                        }
                        v = v * 8 + d;
                        self.i += 1;
                    }
                    if self.i == s {
                        self.i += 1;
                        char_node(self.c[s])
                    } else {
                        Node::Char(v as u16)
                    }
                }
            }
            _ => {
                self.i -= 1;
                if self.unicode {
                    self.check_unicode_escape()?;
                }
                Node::Char(self.char_escape())
            }
        })
    }

    /// In `u` mode, a class's escape at `self.i` (at its `\`) must be
    /// valid (see [`Parser::check_unicode_escape`]).
    fn check_class_escape(&mut self) -> Result<(), String> {
        if !self.unicode || self.peek() != Some('\\') {
            return Ok(());
        }
        match self.c.get(self.i + 1) {
            Some('d' | 'D' | 'w' | 'W' | 's' | 'S' | 'b' | '-' | 'p' | 'P') => Ok(()),
            Some(_) => {
                self.i += 1;
                let r = self.check_unicode_escape();
                self.i -= 1;
                r
            }
            None => Ok(()),
        }
    }

    /// In `u` mode, the escape at `self.i` (past the `\`) must be a
    /// syntax character's, `/`, `-` (in a class) or a valid character
    /// escape.
    fn check_unicode_escape(&self) -> Result<(), String> {
        let e = self.c[self.i];
        let next = |k: usize| self.c.get(self.i + k).copied();
        let hex = |from: usize, n: usize| (0..n).all(|k| next(from + k).is_some_and(|c| c.is_ascii_hexdigit()));
        let ok = match e {
            '^' | '$' | '\\' | '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '/' => true,
            'n' | 'r' | 't' | 'v' | 'f' => true,
            '0' => !next(1).is_some_and(|c| c.is_ascii_digit()),
            'x' => hex(1, 2),
            'u' => {
                hex(1, 4)
                    || next(1) == Some('{') && {
                        let digits = (2..).take_while(|&k| next(k).is_some_and(|c| c.is_ascii_hexdigit())).count();
                        digits > 0 && next(2 + digits) == Some('}')
                    }
            }
            'c' => next(1).is_some_and(|c| c.is_ascii_alphabetic()),
            _ => false,
        };
        if ok { Ok(()) } else { Err(self.error("Invalid escape")) }
    }

    /// The code unit of a character escape at `self.i` (after the `\`).
    fn char_escape(&mut self) -> u16 {
        let e = self.c[self.i];
        self.i += 1;
        let hex = |p: &mut Self, n: usize| {
            let s: String = p.c.get(p.i..p.i + n)?.iter().collect();
            let v = u16::from_str_radix(&s, 16).ok()?;
            p.i += n;
            Some(v)
        };
        match e {
            'n' => 10,
            'r' => 13,
            't' => 9,
            'v' => 11,
            'f' => 12,
            '0' if !self.peek().is_some_and(|c| c.is_ascii_digit()) => 0,
            // A legacy octal escape (Annex B, not `u` mode): up to 0o377.
            '0'..='7' if !self.unicode => {
                let mut v = e.to_digit(8).unwrap();
                // (Three digits at most, leading zeros too; two from `\4`.)
                let mut n = if v < 4 { 2 } else { 1 };
                while let Some(d) = self.peek().and_then(|c| c.to_digit(8)) {
                    if n == 0 {
                        break;
                    }
                    n -= 1;
                    v = v * 8 + d;
                    self.i += 1;
                }
                v as u16
            }
            'x' => hex(self, 2).unwrap_or(b'x' as u16),
            'u' => hex(self, 4).unwrap_or(b'u' as u16),
            'c' => match self.peek() {
                Some(l) if l.is_ascii_alphabetic() => {
                    self.i += 1;
                    (l as u16) % 32
                }
                _ => b'\\' as u16,
            },
            c => {
                let mut b = [0u16; 2];
                c.encode_utf16(&mut b)[0]
            }
        }
    }

    fn class(&mut self) -> Result<Node, String> {
        let neg = self.peek() == Some('^');
        if neg {
            self.i += 1;
        }
        // (Code points: units but in `u` mode.)
        let mut ranges: Vec<(u32, u32)> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("Unterminated character class"));
            };
            if c == ']' {
                self.i += 1;
                break;
            }
            self.check_class_escape()?;
            // One end: a code unit, or a class escape (`\d`: no range).
            let lo = match self.class_atom() {
                Ok(u) => u,
                Err(set) => {
                    if self.unicode && self.peek() == Some('-') && self.c.get(self.i + 1).is_some_and(|&c| c != ']') {
                        return Err(self.error("Invalid character class"));
                    }
                    ranges.extend(set);
                    continue;
                }
            };
            if self.peek() == Some('-') && self.c.get(self.i + 1).is_some_and(|&c| c != ']') {
                self.i += 1;
                self.check_class_escape()?;
                match self.class_atom() {
                    Ok(hi) if hi >= lo => ranges.push((lo, hi)),
                    Ok(_) => return Err(self.error("Range out of order in character class")),
                    Err(_) if self.unicode => return Err(self.error("Invalid character class")),
                    Err(set) => {
                        ranges.push((lo, lo));
                        ranges.push((b'-' as u32, b'-' as u32));
                        ranges.extend(set);
                    }
                }
            } else {
                ranges.push((lo, lo));
            }
        }
        if let Some(m) = self.pending.take() {
            return Err(m);
        }
        if self.unicode {
            return Ok(cp_class(ranges, neg));
        }
        let mut units: Vec<(u16, u16)> = ranges.into_iter().map(|(lo, hi)| (lo as u16, hi as u16)).collect();
        units.sort();
        Ok(Node::Class(units, neg))
    }

    /// A class member: a code point (a code unit but in `u` mode), or
    /// (`Err`) a class escape's ranges.
    fn class_atom(&mut self) -> Result<u32, Vec<(u32, u32)>> {
        let c = self.c[self.i];
        self.i += 1;
        if c != '\\' {
            if self.unicode {
                return Ok(c as u32);
            }
            let mut b = [0u16; 2];
            return Ok(c.encode_utf16(&mut b)[0] as u32);
        }
        let Some(e) = self.peek() else { return Ok(b'\\' as u32) };
        let wide = |r: Vec<(u16, u16)>| r.into_iter().map(|(lo, hi)| (lo as u32, hi as u32)).collect::<Vec<_>>();
        match e {
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                self.i += 1;
                let (r, neg) = class_escape(e);
                Err(match (neg, self.unicode) {
                    (false, _) => wide(r),
                    (true, false) => wide(complement(&r)),
                    (true, true) => complement_points(&wide(r)),
                })
            }
            'p' | 'P' if self.unicode => {
                self.i += 1;
                // (An invalid name: no ranges, the error noted for later.)
                match self.property(e == 'P') {
                    Ok(set) => Err(set),
                    Err(m) => {
                        self.pending.get_or_insert(m);
                        Err(Vec::new())
                    }
                }
            }
            'b' => {
                self.i += 1;
                Ok(8)
            }
            '-' => {
                self.i += 1;
                Ok(b'-' as u32)
            }
            // `\c` and a digit or `_`, in a class (Annex B): a control code.
            'c' if !self.unicode && self.c.get(self.i + 1).is_some_and(|&d| d.is_ascii_digit() || d == '_') => {
                let d = self.c[self.i + 1];
                self.i += 2;
                Ok(d as u32 % 32)
            }
            'u' if self.unicode && self.c.get(self.i + 1) == Some(&'{') => {
                let s = self.i + 2;
                let end = self.c[s..].iter().position(|&c| c == '}').unwrap_or(0);
                let hex: String = self.c[s..s + end].iter().collect();
                self.i = s + end + 1;
                Ok(u32::from_str_radix(&hex, 16).map_or(0, |v| v.min(0x10FFFF)))
            }
            _ => {
                let u = self.char_escape() as u32;
                // (`u` mode: `\uD83D\uDE00`, a surrogate pair's escapes, is
                // the one code point.)
                if self.unicode
                    && (0xD800..0xDC00).contains(&u)
                    && self.c.get(self.i..self.i + 2) == Some(&['\\', 'u'])
                    && let Some(t) = self.c.get(self.i + 2..self.i + 6).and_then(|h| {
                        u32::from_str_radix(&h.iter().collect::<String>(), 16).ok()
                    })
                    && (0xDC00..0xE000).contains(&t)
                {
                    self.i += 6;
                    return Ok(0x10000 + ((u - 0xD800) << 10) + (t - 0xDC00));
                }
                Ok(u)
            }
        }
    }

    /// `\p{…}` (or `\P{…}`, `neg`) at `self.i` (past the `p`), in `u` mode:
    /// the code points of the property (a binary one, a general category,
    /// a script, … by any of their names: see `unicode_props`).
    fn property(&mut self, neg: bool) -> Result<Vec<(u32, u32)>, String> {
        if self.peek() != Some('{') {
            return Err(self.error("Invalid property name"));
        }
        let s = self.i + 1;
        let Some(end) = self.c[s..].iter().position(|&c| c == '}') else {
            return Err(self.error("Invalid property name"));
        };
        let name: String = self.c[s..s + end].iter().collect();
        self.i = s + end + 1;
        let names = &crate::unicode_props::NAMES;
        let Ok(k) = names.binary_search_by(|(n, _)| (*n).cmp(name.as_str())) else {
            return Err(self.error("Invalid property name"));
        };
        let set = crate::unicode_props::SETS[names[k].1 as usize];
        Ok(if neg { complement_points(set) } else { set.to_vec() })
    }
}

/// The code points `0..=0x10FFFF` not in sorted ranges `r`.
fn complement_points(r: &[(u32, u32)]) -> Vec<(u32, u32)> {
    let mut r = r.to_vec();
    r.sort();
    let mut out = Vec::new();
    let mut next = 0;
    for &(lo, hi) in &r {
        if lo > next {
            out.push((next, lo - 1));
        }
        next = next.max(hi + 1);
    }
    if next <= 0x10FFFF {
        out.push((next, 0x10FFFF));
    }
    out
}

/// A class of code points `ranges` (negated if `neg`), in `u` mode: of a
/// unit for the BMP's, and of a surrogate pair for the astral ones' (tried
/// first: an astral character is one, not its halves).
fn cp_class(mut ranges: Vec<(u32, u32)>, neg: bool) -> Node {
    ranges.sort();
    let mut astral: Vec<(u32, u32)> =
        ranges.iter().filter(|r| r.1 >= 0x10000).map(|&(lo, hi)| (lo.max(0x10000), hi.min(0x10FFFF))).collect();
    let mut bmp: Vec<(u16, u16)> =
        ranges.iter().filter(|r| r.0 <= 0xFFFF).map(|&(lo, hi)| (lo as u16, hi.min(0xFFFF) as u16)).collect();
    if neg {
        astral = complement_points(&astral).into_iter().filter(|r| r.1 >= 0x10000).map(|(lo, hi)| (lo.max(0x10000), hi)).collect();
        // (Not a pair's first half: its character is astral.)
        bmp.push((0xD800, 0xDBFF));
        bmp.sort();
    }
    if astral.is_empty() {
        return Node::Class(bmp, neg);
    }
    Node::Alt(vec![Node::Astral(astral), Node::Class(bmp, neg)])
}

/// A surrogate pair: an astral character in `u` mode.
fn surrogate_pair() -> Node {
    Node::Seq(vec![Node::Class(vec![(0xD800, 0xDBFF)], false), Node::Class(vec![(0xDC00, 0xDFFF)], false)])
}

fn char_node(c: char) -> Node {
    let mut b = [0u16; 2];
    match c.encode_utf16(&mut b) {
        [u] => Node::Char(*u),
        [a, b] => Node::Seq(vec![Node::Char(*a), Node::Char(*b)]),
        _ => unreachable!(),
    }
}

/// `\d`, `\w`, `\s` and their negations: the ranges, and whether negated.
fn class_escape(e: char) -> (Vec<(u16, u16)>, bool) {
    let r = match e.to_ascii_lowercase() {
        'd' => vec![(0x30, 0x39)],
        'w' => vec![(0x30, 0x39), (0x41, 0x5A), (0x5F, 0x5F), (0x61, 0x7A)],
        _ => vec![
            (0x09, 0x0D),
            (0x20, 0x20),
            (0xA0, 0xA0),
            (0x1680, 0x1680),
            (0x2000, 0x200A),
            (0x2028, 0x2029),
            (0x202F, 0x202F),
            (0x205F, 0x205F),
            (0x3000, 0x3000),
            (0xFEFF, 0xFEFF),
        ],
    };
    (r, e.is_ascii_uppercase())
}

fn complement(r: &[(u16, u16)]) -> Vec<(u16, u16)> {
    let mut r = r.to_vec();
    r.sort();
    let mut out = Vec::new();
    let mut next = 0u32;
    for &(lo, hi) in &r {
        if (lo as u32) > next {
            out.push((next as u16, lo - 1));
        }
        next = next.max(hi as u32 + 1);
    }
    if next <= 0xFFFF {
        out.push((next as u16, 0xFFFF));
    }
    out
}

/// The astral code point of the surrogate pair at `s[i..]`, if one is.
fn astral_at(s: &[u16], i: usize) -> Option<u32> {
    let (&a, &b) = (s.get(i)?, s.get(i + 1)?);
    ((0xD800..0xDC00).contains(&a) && (0xDC00..0xE000).contains(&b))
        .then(|| 0x10000 + (((a - 0xD800) as u32) << 10) + (b - 0xDC00) as u32)
}

/// Whether `c` is in sorted inclusive ranges `r`.
fn in_points(r: &[(u32, u32)], c: u32) -> bool {
    let k = r.partition_point(|&(_, hi)| hi < c);
    k < r.len() && r[k].0 <= c
}

fn is_line_terminator(c: u16) -> bool {
    matches!(c, 0x0A | 0x0D | 0x2028 | 0x2029)
}

fn is_word(c: u16) -> bool {
    matches!(c, 0x30..=0x39 | 0x41..=0x5A | 0x5F | 0x61..=0x7A)
}

/// `c`'s canonical form for case-insensitive matching (its upper case,
/// when that is one code unit).
fn canonical(c: u16) -> u16 {
    if c < 0x80 {
        return (c as u8).to_ascii_uppercase() as u16;
    }
    match char::from_u32(c as u32) {
        Some(ch) => {
            let mut up = ch.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) if (u as u32) < 0x10000 && !(c >= 0x80 && (u as u32) < 0x80) => u as u16,
                _ => c,
            }
        }
        None => c,
    }
}

/// An instruction of a compiled pattern. Jump targets are indices into
/// `Program::code`; a failing instruction backtracks.
#[derive(Clone, Copy, Debug)]
enum Insn {
    /// One code unit.
    Char(u16),
    /// The code units `Program::strs[k]`, in a row.
    Str(u32),
    /// One code unit, case folded: as given, or with its canonical form.
    CharFold { c: u16, canon: u16 },
    /// `.`: anything but a line terminator.
    Any,
    /// A class: ASCII members by `ascii`'s bits, the rest by
    /// `Program::classes[k]`.
    Class { ascii: u128, k: u32 },
    /// A surrogate pair of a code point in `Program::astral[k]`.
    Astral(u32),
    Start,
    End,
    /// `\b` (true) or `\B`.
    Boundary(bool),
    BackRef(u32),
    /// Continue with the next instruction; on backtracking, at `to`.
    Split(u32),
    Jmp(u32),
    /// Register `r` takes the position.
    Mark(u32),
    /// Group `g` matched from register `r`'s position to here.
    Save { g: u32, r: u32 },
    /// Groups `lo..hi` are cleared (each iteration of a repetition).
    Clear { lo: u32, hi: u32 },
    /// Register `r` takes 0.
    Zero(u32),
    /// A repetition's head, with its count in register `c`: on into the
    /// body (the next instruction) or out to `exit`, greedy or lazy.
    Head { c: u32, min: u32, max: u32, greedy: bool, exit: u32 },
    /// A repetition's body done, having started at register `p`'s
    /// position: an empty iteration past the minimum fails; otherwise the
    /// count goes up, back to `head`.
    Tail { c: u32, p: u32, min: u32, head: u32 },
    /// A greedy repetition of the single-unit instruction that follows:
    /// as many as match (at most `max`), then fewer on backtracking (at
    /// least `min`); on after that instruction.
    Run { min: u32, max: u32 },
    /// A lazy repetition of the single-unit instruction that follows: as
    /// few as `min`, then one more on each backtracking (at most `max`).
    LazyRun { min: u32, max: u32 },
    /// A lookahead, whose body follows and ends in `LookEnd`; on at
    /// `after`.
    Look { negative: bool, after: u32 },
    LookEnd,
    /// A lookbehind: its body (ending in `BehindEnd`) matched backwards
    /// from here, right to left (see `Program::backward`); on at `after`.
    Behind { negative: bool, after: u32 },
    BehindEnd,
    /// A step back, one unit (before and after a unit matched backwards).
    Back,
    /// Group `g` matched backwards, from here to register `r`'s position.
    SaveBack { g: u32, r: u32 },
    /// A backreference matched backwards: the text before here.
    BackRefBack(u32),
    Match,
}

/// A compiled pattern.
#[derive(Default)]
struct Program {
    code: Vec<Insn>,
    /// Classes' ranges and negation, for units past ASCII.
    classes: Vec<(Vec<(u16, u16)>, bool)>,
    /// `Str`'s code units.
    strs: Vec<Vec<u16>>,
    /// `Astral`'s ranges.
    astral: Vec<Vec<(u32, u32)>>,
    /// Registers: positions and repetition counts.
    nregs: usize,
    /// How case compares (`i`, `u`).
    ic: Case,
    /// Compiling a lookbehind's body: matched right to left.
    backward: bool,
}

/// What backtracking pops.
enum Bt {
    /// A choice point: resume at `pc` from `pos`.
    Resume { pc: u32, pos: usize },
    /// A register's earlier value.
    Reg { r: u32, old: usize },
    /// A group's earlier span.
    Cap { g: u32, old: Option<(usize, usize)> },
    /// A greedy run from `base` that matched `n` units: fewer next (at
    /// least `min`), continuing at `pc`.
    Run { pc: u32, min: u32, base: usize, n: usize },
    /// A lazy run that matched `n` units, up to `pos`: one more next (at
    /// most `max`), if the unit (before `pc`) matches there.
    LazyRun { pc: u32, max: u32, pos: usize, n: usize },
    /// A lookahead's body runs: if it fails, continue at `after` from
    /// `pos` when `negative`; `caps` are the captures before it.
    Look(Box<(usize, u32, bool, Captures)>),
    /// The captures before a positive lookahead that matched.
    Caps(Box<Captures>),
}

/// The interpreter's state, kept over an `exec`'s attempts.
#[derive(Default)]
struct Run {
    caps: Captures,
    regs: Vec<usize>,
    stack: Vec<Bt>,
    /// Native code's registers and backtracking stack (see [`Native`]).
    slots: Vec<usize>,
    bt: Vec<usize>,
}

/// The exec at which a pattern is compiled to native code: `CLOISON_NO_JIT`
/// or `CLOISON_NO_RX_JIT` set, never.
fn native_after() -> u32 {
    static AFTER: std::sync::OnceLock<u32> = std::sync::OnceLock::new();
    *AFTER.get_or_init(|| {
        if std::env::var_os("CLOISON_NO_JIT").is_some() || std::env::var_os("CLOISON_NO_RX_JIT").is_some() {
            u32::MAX
        } else {
            NATIVE_AFTER
        }
    })
}

/// Execs of a pattern before it's compiled to native code.
const NATIVE_AFTER: u32 = 3;

/// The native backtracking stack's first size and largest, in words: a
/// match that needs more is the interpreter's.
const NATIVE_STACK: usize = 1 << 10;
const NATIVE_STACK_MAX: usize = 1 << 20;

/// A program as native code (`jit::rx`): one piece per instruction (a run
/// and its unit share one), then `rx_fail`, `rx_nomatch` and the runs'
/// backtracking pieces. Its registers (`Run::slots`): a scratch one (for
/// the entries below a run's), the program's, then two per group.
struct Native {
    code: crate::jit::rx::Code,
    /// The records pieces point at.
    _runs: Box<[crate::jit::rx::Run]>,
    _heads: Box<[crate::jit::rx::Head]>,
    classes: Box<[crate::jit::rx::Class]>,
    _ranges: Vec<Vec<[u16; 2]>>,
    /// `rx_nomatch`'s address.
    nomatch: usize,
    /// The first group's register, and the number of registers.
    caps_at: usize,
    slots: usize,
}

impl Native {
    /// `re`'s program compiled, if it has only instructions that compile
    /// (not backreferences or lookaheads).
    #[cold]
    fn compile(re: &Regex) -> Option<Native> {
        use crate::jit::rx::{self, Imm64, Piece, To};
        // (`rx_fold` folds as without `u`.)
        if Case::of(re) == Case::Unicode {
            return None;
        }
        let code = &re.prog.code;
        // Each instruction's piece (a run's unit: the run's).
        let mut idx = vec![0; code.len() + 1];
        let (mut n, mut nruns, mut nheads) = (0, 0, 0);
        let mut i = 0;
        while i < code.len() {
            idx[i] = n;
            n += 1;
            match code[i] {
                Insn::BackRef(_)
                | Insn::Look { .. }
                | Insn::LookEnd
                | Insn::Behind { .. }
                | Insn::BehindEnd
                | Insn::Back
                | Insn::SaveBack { .. }
                | Insn::BackRefBack(_)
                | Insn::Astral(_) => return None,
                Insn::Run { .. } | Insn::LazyRun { .. } => {
                    idx[i + 1] = idx[i];
                    nruns += 1;
                    i += 2;
                    continue;
                }
                Insn::Head { .. } => nheads += 1,
                _ => {}
            }
            i += 1;
        }
        let caps_at = 1 + re.prog.nregs;
        let slots = caps_at + 2 * re.groups;
        if slots > 0xFFFF {
            return None;
        }
        let (fail, nomatch) = (n, n + 1);
        let mut runs: Box<[rx::Run]> = (0..nruns).map(|_| rx::Run::default()).collect();
        let mut heads: Box<[rx::Head]> = (0..nheads).map(|_| rx::Head::default()).collect();
        let ranges: Vec<Vec<[u16; 2]>> =
            re.prog.classes.iter().map(|(r, _)| r.iter().map(|&(lo, hi)| [lo, hi]).collect()).collect();
        let classes: Box<[rx::Class]> = re
            .prog
            .classes
            .iter()
            .zip(&ranges)
            .map(|((_, neg), r)| rx::Class { ranges: r.as_ptr(), n: r.len(), neg: *neg as usize })
            .collect();
        let reg = |r: u32| 1 + r as usize;
        let group = |g: u32| caps_at + 2 * g as usize;
        let piece = |name| Piece::new(name, To::Piece(fail));
        let mut pieces = Vec::with_capacity(n + 2 + nruns);
        let mut backs = Vec::with_capacity(nruns);
        // (head, its exit's piece, its body's piece)
        let mut head_at = Vec::with_capacity(nheads);
        let (mut runs_k, mut heads_k) = (0, 0);
        let mut i = 0;
        while i < code.len() {
            let p = match code[i] {
                Insn::Char(c) => Piece { imm: c as u32, ..piece("rx_char") },
                Insn::CharFold { c, canon } => Piece { imm: c as u32 | (canon as u32) << 16, ..piece("rx_fold") },
                Insn::Any => piece("rx_any"),
                Insn::Class { ascii, k } => Piece {
                    imm: k,
                    imm64: Imm64::Value(ascii as u64),
                    imm64b: (ascii >> 64) as u64,
                    ..piece("rx_class")
                },
                Insn::Str(k) => {
                    let u = &re.prog.strs[k as usize];
                    Piece { imm: u.len() as u32, imm64: Imm64::Value(u.as_ptr() as u64), ..piece("rx_str") }
                }
                Insn::Start => piece(if re.multiline { "rx_start_m" } else { "rx_start" }),
                Insn::End => piece(if re.multiline { "rx_end_m" } else { "rx_end" }),
                Insn::Boundary(b) => Piece { imm: b as u32, ..piece("rx_boundary") },
                Insn::Split(to) => Piece { imm64: Imm64::Piece(idx[to as usize]), ..piece("rx_split") },
                Insn::Jmp(to) => Piece { cont: To::Piece(idx[to as usize]), ..piece("rx_jmp") },
                Insn::Mark(r) => Piece { imm: reg(r) as u32, ..piece("rx_mark") },
                Insn::Zero(r) => Piece { imm: reg(r) as u32, ..piece("rx_zero") },
                Insn::Save { g, r } => Piece { imm: (group(g) | reg(r) << 16) as u32, ..piece("rx_save") },
                Insn::Clear { lo, hi } => Piece { imm: (group(lo) | group(hi) << 16) as u32, ..piece("rx_clear") },
                Insn::Head { c, min, max, greedy, exit } => {
                    let h = &mut heads[heads_k];
                    (h.c, h.min, h.max) = (reg(c), min as usize, max as usize);
                    head_at.push((heads_k, idx[exit as usize], idx[i + 1]));
                    heads_k += 1;
                    Piece {
                        imm64: Imm64::Value(h as *const rx::Head as u64),
                        ..piece(if greedy { "rx_head" } else { "rx_lazy_head" })
                    }
                }
                Insn::Tail { c, p, min, head } => Piece {
                    imm: (reg(c) | reg(p) << 16) as u32,
                    imm64: Imm64::Value(min as u64),
                    cont: To::Piece(idx[head as usize]),
                    ..piece("rx_tail")
                },
                Insn::Run { min, max } | Insn::LazyRun { min, max } => {
                    let greedy = matches!(code[i], Insn::Run { .. });
                    let r = &mut runs[runs_k];
                    runs_k += 1;
                    (r.min, r.max) = (min as usize, max as usize);
                    let names = match code[i + 1] {
                        Insn::Char(c) => {
                            r.k = c as usize;
                            ["rx_run_char", "rx_lazy_char", "rx_lazyback_char"]
                        }
                        Insn::CharFold { c, canon } => {
                            r.k = c as usize | (canon as usize) << 16;
                            ["rx_run_fold", "rx_lazy_fold", "rx_lazyback_fold"]
                        }
                        Insn::Any => ["rx_run_any", "rx_lazy_any", "rx_lazyback_any"],
                        Insn::Class { ascii, k } => {
                            (r.lo, r.hi, r.k) = (ascii as u64, (ascii >> 64) as u64, k as usize);
                            ["rx_run_class", "rx_lazy_class", "rx_lazyback_class"]
                        }
                        i => unreachable!("a run of {i:?}"),
                    };
                    let rec = Imm64::Value(r as *const rx::Run as u64);
                    let back = if greedy { "rx_back" } else { names[2] };
                    backs.push(Piece { imm64: rec, cont: To::Piece(idx[i + 2]), ..piece(back) });
                    pieces.push(Piece { imm64: rec, ..piece(if greedy { names[0] } else { names[1] }) });
                    i += 2;
                    continue;
                }
                Insn::Match => piece("rx_match"),
                Insn::BackRef(_)
                | Insn::Look { .. }
                | Insn::LookEnd
                | Insn::Behind { .. }
                | Insn::BehindEnd
                | Insn::Back
                | Insn::SaveBack { .. }
                | Insn::BackRefBack(_)
                | Insn::Astral(_) => unreachable!(),
            };
            pieces.push(p);
            i += 1;
        }
        debug_assert_eq!(pieces.len(), n);
        pieces.push(piece("rx_fail"));
        pieces.push(piece("rx_nomatch"));
        pieces.extend(backs);
        let native = rx::Code::assemble(&pieces);
        for (k, r) in runs.iter_mut().enumerate() {
            r.back = native.at[nomatch + 1 + k];
        }
        for (k, exit, body) in head_at {
            (heads[k].exit, heads[k].body) = (native.at[exit], native.at[body]);
        }
        Some(Native {
            nomatch: native.at[nomatch],
            code: native,
            _runs: runs,
            _heads: heads,
            classes,
            _ranges: ranges,
            caps_at,
            slots,
        })
    }

    /// `run`'s registers and stack set up for attempts: every group unset,
    /// and at the stack's bottom, the entry that ends a failed attempt.
    fn begin(&self, run: &mut Run) {
        let Run { slots, bt, .. } = run;
        if slots.len() != self.slots {
            slots.resize(self.slots, 0);
        }
        slots[self.caps_at..].fill(crate::jit::rx::NONE);
        if bt.len() < NATIVE_STACK {
            bt.resize(NATIVE_STACK, 0);
        }
        bt[0] = self.nomatch;
    }

    /// An attempt at `at` (after [`Native::begin`], and attempts that
    /// failed), as [`Program::run`]: whether it matched, or `None` if it's
    /// the interpreter's (the stack filled, or a unit past ASCII needs
    /// folding). That leaves registers changed.
    fn run(&self, re: &Regex, s: &[u16], at: usize, run: &mut Run) -> Option<bool> {
        use crate::jit::rx;
        let Run { caps, slots, bt, .. } = run;
        let ctx = rx::Ctx {
            limit: unsafe { bt.as_mut_ptr().add(bt.len() - 4) },
            classes: self.classes.as_ptr(),
            ic: re.ignore_case as usize,
        };
        let end = unsafe {
            (self.code.entry())(s.as_ptr(), s.len(), at, bt.as_mut_ptr().add(2), slots.as_mut_ptr(), &ctx)
        };
        match end {
            rx::NOMATCH => Some(false),
            rx::BAIL => {
                if bt.len() < NATIVE_STACK_MAX {
                    bt.resize(bt.len() * 2, 0);
                }
                None
            }
            end => {
                caps[0] = Some((at, end));
                for (g, c) in caps.iter_mut().enumerate().skip(1) {
                    let k = self.caps_at + 2 * g;
                    *c = (slots[k] != rx::NONE).then(|| (slots[k], slots[k + 1]));
                }
                Some(true)
            }
        }
    }
}

impl Program {
    fn compile(re: &Regex) -> Program {
        let mut p = Program { ic: Case::of(re), ..Program::default() };
        p.emit(re, &re.node);
        p.code.push(Insn::Match);
        p
    }

    fn reg(&mut self) -> u32 {
        self.nregs += 1;
        (self.nregs - 1) as u32
    }

    fn here(&self) -> u32 {
        self.code.len() as u32
    }

    /// The instruction matching the single unit `n` stands for, if it does.
    fn unit(&mut self, re: &Regex, n: &Node) -> Option<Insn> {
        let ic = Case::of(re);
        Some(match n {
            Node::Char(c) if ic != Case::Exact => Insn::CharFold { c: *c, canon: ic.canon(*c) },
            Node::Char(c) => Insn::Char(*c),
            Node::Any => Insn::Any,
            Node::Class(r, neg) => {
                let ascii = (0..128u16).filter(|&x| in_class(ic, r, *neg, x)).fold(0u128, |m, x| m | 1 << x);
                self.classes.push((r.clone(), *neg));
                Insn::Class { ascii, k: (self.classes.len() - 1) as u32 }
            }
            _ => return None,
        })
    }

    fn emit(&mut self, re: &Regex, n: &Node) {
        if let Some(i) = self.unit(re, n) {
            // (Backwards: the unit before here.)
            if self.backward {
                self.code.extend([Insn::Back, i, Insn::Back]);
            } else {
                self.code.push(i);
            }
            return;
        }
        match n {
            Node::Empty => {}
            Node::Char(_) | Node::Any | Node::Class(..) => unreachable!(),
            Node::Astral(r) => {
                self.astral.push(r.clone());
                let i = Insn::Astral((self.astral.len() - 1) as u32);
                if self.backward {
                    self.code.extend([Insn::Back, Insn::Back, i, Insn::Back, Insn::Back]);
                } else {
                    self.code.push(i);
                }
            }
            Node::Start => self.code.push(Insn::Start),
            Node::End => self.code.push(Insn::End),
            Node::Boundary(b) => self.code.push(Insn::Boundary(*b)),
            Node::BackRef(g) if self.backward => self.code.push(Insn::BackRefBack(*g as u32)),
            Node::BackRef(g) => self.code.push(Insn::BackRef(*g as u32)),
            Node::Group(inner, None) => self.emit(re, inner),
            Node::Group(inner, Some(g)) => {
                let r = self.reg();
                self.code.push(Insn::Mark(r));
                self.emit(re, inner);
                self.code.push(if self.backward { Insn::SaveBack { g: *g as u32, r } } else { Insn::Save { g: *g as u32, r } });
            }
            // A lookahead's body forwards, a lookbehind's backwards, wherever
            // they are.
            Node::Look(inner, positive, false) => {
                let at = self.code.len();
                self.code.push(Insn::Look { negative: !positive, after: 0 });
                let backward = std::mem::replace(&mut self.backward, false);
                self.emit(re, inner);
                self.backward = backward;
                self.code.push(Insn::LookEnd);
                let after = self.here();
                self.code[at] = Insn::Look { negative: !positive, after };
            }
            Node::Look(inner, positive, true) => {
                let at = self.code.len();
                self.code.push(Insn::Behind { negative: !positive, after: 0 });
                let backward = std::mem::replace(&mut self.backward, true);
                self.emit(re, inner);
                self.backward = backward;
                self.code.push(Insn::BehindEnd);
                let after = self.here();
                self.code[at] = Insn::Behind { negative: !positive, after };
            }
            Node::Alt(alts) => {
                let mut jumps = Vec::new();
                for (k, alt) in alts.iter().enumerate() {
                    let split = self.code.len();
                    if k + 1 < alts.len() {
                        self.code.push(Insn::Split(0));
                    }
                    self.emit(re, alt);
                    if k + 1 < alts.len() {
                        jumps.push(self.code.len());
                        self.code.push(Insn::Jmp(0));
                        self.code[split] = Insn::Split(self.here());
                    }
                }
                let end = self.here();
                for j in jumps {
                    self.code[j] = Insn::Jmp(end);
                }
            }
            // Backwards, the last first, unit by unit.
            Node::Seq(nodes) if self.backward => {
                for n in nodes.iter().rev() {
                    self.emit(re, n);
                }
            }
            Node::Seq(nodes) => {
                let mut k = 0;
                while k < nodes.len() {
                    // Two or more characters (case not folded): one `Str`.
                    let run = nodes[k..].iter().take_while(|n| matches!(n, Node::Char(_))).count();
                    if run >= 2 && !re.ignore_case {
                        let units = nodes[k..k + run]
                            .iter()
                            .map(|n| match n {
                                Node::Char(c) => *c,
                                _ => unreachable!(),
                            })
                            .collect();
                        self.strs.push(units);
                        self.code.push(Insn::Str((self.strs.len() - 1) as u32));
                        k += run;
                    } else {
                        self.emit(re, &nodes[k]);
                        k += 1;
                    }
                }
            }
            Node::Repeat(inner, min, max, greedy, groups) => {
                // (A run goes forwards.)
                if !self.backward
                    && let Some(i) = self.unit(re, inner)
                {
                    let (min, max) = (*min, *max);
                    self.code.push(if *greedy { Insn::Run { min, max } } else { Insn::LazyRun { min, max } });
                    self.code.push(i);
                    return;
                }
                // A body that can't match empty, repeated at most once or
                // without bound: no count, no empty-iteration check.
                if width(inner).0 > 0 && *min <= 1 && (*max == 1 || *max == u32::MAX) && *max >= *min {
                    if *min == 1 {
                        self.emit(re, inner);
                        if *max == 1 {
                            return;
                        }
                    }
                    let top = self.here();
                    let split = self.code.len();
                    if *greedy {
                        self.code.push(Insn::Split(0));
                    } else {
                        self.code.push(Insn::Split(split as u32 + 2));
                        self.code.push(Insn::Jmp(0));
                    }
                    if !groups.is_empty() && *max != 1 {
                        self.code.push(Insn::Clear { lo: groups.start as u32, hi: groups.end as u32 });
                    }
                    self.emit(re, inner);
                    if *max != 1 {
                        self.code.push(Insn::Jmp(top));
                    }
                    let exit = self.here();
                    if *greedy {
                        self.code[split] = Insn::Split(exit);
                    } else {
                        self.code[split + 1] = Insn::Jmp(exit);
                    }
                    return;
                }
                let (c, p) = (self.reg(), self.reg());
                self.code.push(Insn::Zero(c));
                let head = self.here();
                self.code.push(Insn::Head { c, min: *min, max: *max, greedy: *greedy, exit: 0 });
                self.code.push(Insn::Mark(p));
                if !groups.is_empty() {
                    self.code.push(Insn::Clear { lo: groups.start as u32, hi: groups.end as u32 });
                }
                self.emit(re, inner);
                self.code.push(Insn::Tail { c, p, min: *min, head });
                let exit = self.here();
                self.code[head as usize] = Insn::Head { c, min: *min, max: *max, greedy: *greedy, exit };
            }
        }
    }

    /// Whether the single-unit instruction `i` matches `c`.
    #[inline(always)]
    fn unit_matches(&self, i: Insn, c: u16) -> bool {
        match i {
            Insn::Char(x) => c == x,
            Insn::CharFold { c: x, canon } => c == x || self.ic.canon(c) == canon,
            Insn::Any => !is_line_terminator(c),
            Insn::Class { ascii, k } => {
                if c < 128 {
                    ascii >> c & 1 != 0
                } else {
                    let (r, neg) = &self.classes[k as usize];
                    in_class(self.ic, r, *neg, c)
                }
            }
            _ => unreachable!(),
        }
    }

    /// Run from `start` over `s`: whether a match begins there, its
    /// captures in `run.caps`.
    // (Not inlined into `exec_into`: its frame would make every exec's
    // setup cost more, natively run or not.)
    #[inline(never)]
    fn run(&self, re: &Regex, s: &[u16], start: usize, run: &mut Run) -> bool {
        let Run { caps, regs, stack, .. } = run;
        caps.fill(None);
        stack.clear();
        let (mut pc, mut pos) = (0usize, start);
        loop {
            let ok = match self.code[pc] {
                i @ (Insn::Char(_) | Insn::CharFold { .. } | Insn::Any | Insn::Class { .. }) => {
                    let ok = pos < s.len() && self.unit_matches(i, s[pos]);
                    pos += 1;
                    pc += 1;
                    ok
                }
                Insn::Str(k) => {
                    let u = &self.strs[k as usize];
                    let ok = s.get(pos..pos + u.len()) == Some(&u[..]);
                    pos += u.len();
                    pc += 1;
                    ok
                }
                Insn::Astral(k) => {
                    pc += 1;
                    let ok = pos + 1 < s.len() && astral_at(s, pos).is_some_and(|c| in_points(&self.astral[k as usize], c));
                    pos += 2;
                    ok
                }
                Insn::Start => {
                    pc += 1;
                    pos == 0 || re.multiline && is_line_terminator(s[pos - 1])
                }
                Insn::End => {
                    pc += 1;
                    pos == s.len() || re.multiline && is_line_terminator(s[pos])
                }
                Insn::Boundary(want) => {
                    pc += 1;
                    let a = pos > 0 && self.ic.word(s[pos - 1]);
                    let b = pos < s.len() && self.ic.word(s[pos]);
                    (a != b) == want
                }
                Insn::BackRef(g) => {
                    pc += 1;
                    match caps[g as usize] {
                        None => true,
                        Some((a, b)) => {
                            let n = b - a;
                            let ok = pos + n <= s.len() && (0..n).all(|d| eq(Case::of(re), s[pos + d], s[a + d]));
                            pos += n;
                            ok
                        }
                    }
                }
                Insn::Split(to) => {
                    stack.push(Bt::Resume { pc: to, pos });
                    pc += 1;
                    true
                }
                Insn::Jmp(to) => {
                    pc = to as usize;
                    true
                }
                Insn::Mark(r) => {
                    stack.push(Bt::Reg { r, old: regs[r as usize] });
                    regs[r as usize] = pos;
                    pc += 1;
                    true
                }
                Insn::Save { g, r } => {
                    stack.push(Bt::Cap { g, old: caps[g as usize] });
                    caps[g as usize] = Some((regs[r as usize], pos));
                    pc += 1;
                    true
                }
                Insn::Clear { lo, hi } => {
                    for g in lo..hi {
                        if caps[g as usize].is_some() {
                            stack.push(Bt::Cap { g, old: caps[g as usize].take() });
                        }
                    }
                    pc += 1;
                    true
                }
                Insn::Zero(r) => {
                    stack.push(Bt::Reg { r, old: regs[r as usize] });
                    regs[r as usize] = 0;
                    pc += 1;
                    true
                }
                Insn::Head { c, min, max, greedy, exit } => {
                    let count = regs[c as usize] as u32;
                    let (more, done) = (count < max, count >= min);
                    let (body, exit) = (pc as u32 + 1, exit);
                    // Greedy: another iteration first, then out; lazy: out
                    // first.
                    let (first, second) = if greedy { (more.then_some(body), done.then_some(exit)) } else { (done.then_some(exit), more.then_some(body)) };
                    match (first, second) {
                        (Some(a), Some(b)) => {
                            stack.push(Bt::Resume { pc: b, pos });
                            pc = a as usize;
                            true
                        }
                        (Some(a), None) | (None, Some(a)) => {
                            pc = a as usize;
                            true
                        }
                        (None, None) => false,
                    }
                }
                Insn::Tail { c, p, min, head } => {
                    let count = regs[c as usize];
                    if pos == regs[p as usize] && count >= min as usize {
                        false
                    } else {
                        stack.push(Bt::Reg { r: c, old: count });
                        regs[c as usize] = count + 1;
                        pc = head as usize;
                        true
                    }
                }
                Insn::Run { min, max } => {
                    let unit = self.code[pc + 1];
                    let limit = (s.len() - pos.min(s.len())).min(max as usize);
                    let n = s[pos..pos + limit].iter().take_while(|&&c| self.unit_matches(unit, c)).count();
                    let min = min as usize;
                    pc += 2;
                    if n < min {
                        false
                    } else {
                        if n > min {
                            stack.push(Bt::Run { pc: pc as u32, min: min as u32, base: pos, n });
                        }
                        pos += n;
                        true
                    }
                }
                Insn::LazyRun { min, max } => {
                    let unit = self.code[pc + 1];
                    let (min, max) = (min as usize, max as usize);
                    pc += 2;
                    let ok = pos + min <= s.len() && s[pos..pos + min].iter().all(|&c| self.unit_matches(unit, c));
                    pos += min;
                    if ok && min < max {
                        stack.push(Bt::LazyRun { pc: pc as u32, max: max as u32, pos, n: min });
                    }
                    ok
                }
                Insn::Look { negative, after } | Insn::Behind { negative, after } => {
                    stack.push(Bt::Look(Box::new((pos, after, negative, caps.clone()))));
                    pc += 1;
                    true
                }
                Insn::Back => {
                    pc += 1;
                    match pos.checked_sub(1) {
                        Some(p) => {
                            pos = p;
                            true
                        }
                        None => false,
                    }
                }
                Insn::SaveBack { g, r } => {
                    stack.push(Bt::Cap { g, old: caps[g as usize] });
                    caps[g as usize] = Some((pos, regs[r as usize]));
                    pc += 1;
                    true
                }
                Insn::BackRefBack(g) => {
                    pc += 1;
                    match caps[g as usize] {
                        None => true,
                        Some((a, b)) => {
                            let n = b - a;
                            let ok = n <= pos && (0..n).all(|d| eq(Case::of(re), s[pos - n + d], s[a + d]));
                            pos = pos.saturating_sub(n);
                            ok
                        }
                    }
                }
                Insn::LookEnd | Insn::BehindEnd => {
                    // The body matched: its choice points go (a lookahead
                    // matches once), its captures stay.
                    let (at, after, negative, saved) = loop {
                        match stack.pop() {
                            Some(Bt::Look(l)) => break *l,
                            Some(_) => {}
                            None => unreachable!("a lookahead's end without its start"),
                        }
                    };
                    if negative {
                        *caps = saved;
                        false
                    } else {
                        stack.push(Bt::Caps(Box::new(saved)));
                        (pc, pos) = (after as usize, at);
                        true
                    }
                }
                Insn::Match => {
                    caps[0] = Some((start, pos));
                    return true;
                }
            };
            if ok {
                continue;
            }
            // Backtrack: undo down to the latest choice point.
            loop {
                match stack.pop() {
                    None => return false,
                    Some(Bt::Resume { pc: to, pos: at }) => {
                        (pc, pos) = (to as usize, at);
                        break;
                    }
                    Some(Bt::Reg { r, old }) => regs[r as usize] = old,
                    Some(Bt::Cap { g, old }) => caps[g as usize] = old,
                    Some(Bt::Run { pc: to, min, base, n }) => {
                        let n = n - 1;
                        if n > min as usize {
                            stack.push(Bt::Run { pc: to, min, base, n });
                        }
                        (pc, pos) = (to as usize, base + n);
                        break;
                    }
                    Some(Bt::LazyRun { pc: to, max, pos: at, n }) => {
                        let unit = self.code[to as usize - 1];
                        if at < s.len() && self.unit_matches(unit, s[at]) {
                            if n + 1 < max as usize {
                                stack.push(Bt::LazyRun { pc: to, max, pos: at + 1, n: n + 1 });
                            }
                            (pc, pos) = (to as usize, at + 1);
                            break;
                        }
                    }
                    // The body failed.
                    Some(Bt::Look(l)) => {
                        let (at, after, negative, _) = *l;
                        if negative {
                            (pc, pos) = (after as usize, at);
                            break;
                        }
                    }
                    Some(Bt::Caps(saved)) => *caps = *saved,
                }
            }
        }
    }
}

/// The matcher `Program` replaced, over the pattern's tree, passing
/// continuations: the tests check the interpreter against it.
#[cfg(test)]
mod reference {
    use super::*;

    impl Regex {
        /// [`Regex::exec`], by the reference matcher.
        pub(super) fn exec_reference(&self, s: &[u16], start: usize) -> Option<Captures> {
            let mut m = Matcher {
                re: self,
                s,
                caps: vec![None; self.groups],
            };
            // At every position (no skipping, which `exec` does).
            (start..=s.len()).find(|&at| {
                for c in m.caps.iter_mut() {
                    *c = None;
                }
                m.node(&self.node, at, &mut |m, end| {
                    m.caps[0] = Some((at, end));
                    true
                })
            })?;
            Some(m.caps)
        }
    }

struct Matcher<'a> {
    re: &'a Regex,
    s: &'a [u16],
    caps: Captures,
}

type K<'k, 'a> = &'k mut dyn FnMut(&mut Matcher<'a>, usize) -> bool;

impl<'a> Matcher<'a> {
    fn eq(&self, a: u16, b: u16) -> bool {
        eq(Case::of(self.re), a, b)
    }

    fn in_class(&self, ranges: &[(u16, u16)], neg: bool, c: u16) -> bool {
        in_class(Case::of(self.re), ranges, neg, c)
    }

    fn node(&mut self, n: &Node, i: usize, k: K<'_, 'a>) -> bool {
        let s = self.s;
        match n {
            Node::Empty => k(self, i),
            Node::Char(c) => i < s.len() && self.eq(s[i], *c) && k(self, i + 1),
            Node::Any => i < s.len() && !is_line_terminator(s[i]) && k(self, i + 1),
            Node::Class(r, neg) => i < s.len() && self.in_class(r, *neg, s[i]) && k(self, i + 1),
            Node::Astral(r) => astral_at(s, i).is_some_and(|c| in_points(r, c)) && k(self, i + 2),
            Node::Start => {
                (i == 0 || (self.re.multiline && is_line_terminator(s[i - 1]))) && k(self, i)
            }
            Node::End => {
                (i == s.len() || (self.re.multiline && is_line_terminator(s[i]))) && k(self, i)
            }
            Node::Boundary(want) => {
                let a = i > 0 && is_word(s[i - 1]);
                let b = i < s.len() && is_word(s[i]);
                ((a != b) == *want) && k(self, i)
            }
            Node::Group(inner, None) => self.node(inner, i, k),
            Node::Group(inner, Some(g)) => {
                let g = *g;
                self.node(inner, i, &mut |m, j| {
                    let prev = m.caps[g];
                    m.caps[g] = Some((i, j));
                    if k(m, j) {
                        return true;
                    }
                    m.caps[g] = prev;
                    false
                })
            }
            Node::Look(inner, positive, behind) => {
                let saved = self.caps.clone();
                // (A lookbehind's body forwards from each start, the nearest
                // first: its captures not the spec's, which `Program`'s
                // matches backwards, `Insn::Back`.)
                let found = if *behind {
                    (0..=i).rev().any(|k| self.node(inner, k, &mut |_, end| end == i))
                } else {
                    self.node(inner, i, &mut |_, _| true)
                };
                if found == *positive {
                    if k(self, i) {
                        return true;
                    }
                }
                self.caps = saved;
                false
            }
            Node::BackRef(g) => {
                let Some((a, b)) = self.caps.get(*g).copied().flatten() else {
                    return k(self, i);
                };
                let n = b - a;
                i + n <= s.len()
                    && (0..n).all(|d| self.eq(s[i + d], s[a + d]))
                    && k(self, i + n)
            }
            // A failed match leaves the captures as they were (a group
            // restores its own when `k` fails), so nothing is saved here.
            Node::Alt(alts) => alts.iter().any(|alt| self.node(alt, i, k)),
            Node::Seq(nodes) => self.seq(nodes, i, k),
            Node::Repeat(inner, min, max, greedy, groups) => {
                self.repeat(inner, *min, *max, *greedy, groups, 0, i, k)
            }
        }
    }

    fn seq(&mut self, nodes: &[Node], i: usize, k: K<'_, 'a>) -> bool {
        match nodes {
            [] => k(self, i),
            [n] => self.node(n, i, k),
            [n, rest @ ..] => self.node(n, i, &mut |m, j| m.seq(rest, j, k)),
        }
    }

    /// `inner` repeated `count` times so far, from `i`.
    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &mut self,
        inner: &Node,
        min: u32,
        max: u32,
        greedy: bool,
        groups: &std::ops::Range<usize>,
        count: u32,
        i: usize,
        k: K<'_, 'a>,
    ) -> bool {
        // A single character or class repeated greedily: count the run,
        // then try the rest from the longest down (no recursion per char).
        if greedy && let Some(len) = self.single_run(inner, i, max - count) {
            let mut n = len;
            loop {
                if count + n as u32 >= min && k(self, i + n) {
                    return true;
                }
                if n == 0 {
                    return false;
                }
                n -= 1;
            }
        }
        // Each iteration starts with the groups inside cleared, and puts
        // them back if it fails (a failed match changes no captures).
        let more = |m: &mut Self, k: K<'_, 'a>| {
            if count >= max {
                return false;
            }
            let saved = (!groups.is_empty()).then(|| m.caps[groups.clone()].to_vec());
            m.caps[groups.clone()].fill(None);
            let found = m.node(inner, i, &mut |m, j| {
                // An empty iteration past the minimum ends the loop.
                if j == i && count >= min {
                    return false;
                }
                m.repeat(inner, min, max, greedy, groups, count + 1, j, k)
            });
            if !found && let Some(saved) = saved {
                m.caps[groups.clone()].copy_from_slice(&saved);
            }
            found
        };
        if greedy {
            more(self, k) || (count >= min && k(self, i))
        } else {
            (count >= min && k(self, i)) || more(self, k)
        }
    }

    /// For a node matching one code unit with no captures: how many in a
    /// row match from `i` (at most `max`).
    fn single_run(&self, n: &Node, i: usize, max: u32) -> Option<usize> {
        let s = self.s;
        let one = |c: u16| match n {
            Node::Char(x) => self.eq(c, *x),
            Node::Any => !is_line_terminator(c),
            Node::Class(r, neg) => self.in_class(r, *neg, c),
            _ => unreachable!(),
        };
        if !matches!(n, Node::Char(_) | Node::Any | Node::Class(..)) {
            return None;
        }
        let limit = (s.len() - i).min(max as usize);
        Some(s[i..i + limit].iter().take_while(|&&c| one(c)).count())
    }
}
}

#[cfg(test)]
mod tests {
    use super::*;

    fn find(p: &str, f: &str, s: &str) -> Option<Vec<Option<String>>> {
        let re = Regex::new(p, f).unwrap();
        let u: Vec<u16> = s.encode_utf16().collect();
        re.exec(&u, 0).map(|caps| {
            caps.iter()
                .map(|c| c.map(|(a, b)| String::from_utf16_lossy(&u[a..b])))
                .collect()
        })
    }

    #[test]
    fn matches_what_ecmascript_does() {
        let one = |p, f, s| find(p, f, s).map(|c| c[0].clone().unwrap());
        assert_eq!(one("a+b", "", "xaab").as_deref(), Some("aab"));
        assert_eq!(one("^\\s+|\\s+$", "", "  x").as_deref(), Some("  "));
        assert_eq!(one("[\\d.]+\\sTf", "", "12.5 Tf").as_deref(), Some("12.5 Tf"));
        assert_eq!(one("a.*?b", "", "aXbYb").as_deref(), Some("aXb"));
        assert_eq!(one("a.*b", "", "aXbYb").as_deref(), Some("aXbYb"));
        assert_eq!(one("(a|ab)(c|bcd)(d*)", "", "abcd").as_deref(), Some("abcd"));
        assert_eq!(one("\\bfoo\\b", "", "a foo b").as_deref(), Some("foo"));
        assert_eq!(one("FOO", "i", "a foo").as_deref(), Some("foo"));
        assert_eq!(one("^b", "m", "a\nb").as_deref(), Some("b"));
        assert_eq!(one("x(?=y)", "", "xzxy").as_deref(), Some("x"));
        assert_eq!(one("x(?!y)", "", "xyxz").as_deref(), Some("x"));
        assert_eq!(one("(\\w)\\1", "", "abccd").as_deref(), Some("cc"));
        assert_eq!(one("a{2,3}", "", "aaaa").as_deref(), Some("aaa"));
        assert_eq!(one("[^a-c]+", "", "abcxyzab").as_deref(), Some("xyz"));
        assert_eq!(one("\\\\", "", "a\\b").as_deref(), Some("\\"));
        assert_eq!(one("q", "", "abc"), None);
        let c = find("(\\w+)@(\\w+)\\.com", "", "mail bob@site.com now").unwrap();
        assert_eq!(c[1].as_deref(), Some("bob"));
        assert_eq!(c[2].as_deref(), Some("site"));
        assert_eq!(find("(a)|(b)", "", "b").unwrap()[1], None);
        // A repetition's iteration clears the groups inside it.
        assert_eq!(find("((a)|b)+", "", "ab").unwrap()[2], None);
        assert_eq!(find("(z)((a+)?(b+)?(c))*", "", "zaacbbbcac").unwrap()[3..], [Some("a".into()), None, Some("c".into())]);
        assert_eq!(find("(?:(a)|b)*", "", "ab").unwrap()[1], None);
        assert_eq!(find("(a)*", "", "b").unwrap()[1], None);
        assert_eq!(find("(?:(a)|(b))+?c", "", "abc").unwrap()[1..], [None, Some("b".into())]);
    }

    /// A long match takes no native stack per character (the reference
    /// matcher overflows the stack on these).
    #[test]
    fn long_matches_dont_recurse() {
        assert_eq!(std::mem::size_of::<Bt>(), 32);
        let s: Vec<u16> = "ab".repeat(1 << 20).encode_utf16().collect();
        let all = Some((0, s.len()));
        assert_eq!(Regex::new("^(?:a|b)*$", "").unwrap().exec(&s, 0).unwrap()[0], all);
        let c = Regex::new("^(ab)+$", "").unwrap().exec(&s, 0).unwrap();
        assert_eq!((c[0], c[1]), (all, Some((s.len() - 2, s.len()))));
    }

    /// The interpreter finds what the reference matcher does, on random
    /// patterns and strings, from every position.
    #[test]
    fn the_interpreter_agrees_with_the_reference() {
        let mut seed = 0x2545_f491_4f6c_dd1du64;
        let mut rand = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        fn pattern(rand: &mut dyn FnMut(usize) -> usize, depth: u32) -> String {
            let atoms = ["a", "b", "c", "A", ".", "[ab]", "[^a]", "[a-c]", "\\d", "\\w", "\\s", "\\n"];
            let atom = |rand: &mut dyn FnMut(usize) -> usize| atoms[rand(atoms.len())].to_string();
            let quant = |rand: &mut dyn FnMut(usize) -> usize, a: String| {
                let q = ["*", "+", "?", "{0,2}", "{1,3}", "{2}", "{2,}", ""][rand(8)];
                let lazy = if !q.is_empty() && rand(3) == 0 { "?" } else { "" };
                format!("{a}{q}{lazy}")
            };
            if depth == 0 {
                let a = atom(rand);
                return quant(rand, a);
            }
            match rand(12) {
                0 | 1 => {
                    let a = atom(rand);
                    quant(rand, a)
                }
                2 => {
                    let a = format!("({})", pattern(rand, depth - 1));
                    quant(rand, a)
                }
                3 => {
                    let a = format!("(?:{})", pattern(rand, depth - 1));
                    quant(rand, a)
                }
                4 => format!("(?={})", pattern(rand, depth - 1)),
                5 => format!("(?!{})", pattern(rand, depth - 1)),
                6 => format!("{}|{}", pattern(rand, depth - 1), pattern(rand, depth - 1)),
                7 => ["^", "$", "\\b", "\\B"][rand(4)].to_string(),
                8 => format!("\\{}", 1 + rand(3)),
                _ => format!("{}{}", pattern(rand, depth - 1), pattern(rand, depth - 1)),
            }
        }
        let (mut tried, mut matched, mut native) = (0, 0, 0);
        for k in 0..20_000 {
            let p = pattern(&mut rand, 3);
            let f = ["", "i", "m", "im"][rand(4)];
            let Ok(re) = Regex::new(&p, f) else { continue };
            // Every other pattern as native code, when it compiles.
            if k % 2 == 0 {
                let n = Native::compile(&re);
                native += n.is_some() as usize;
                let _ = re.native.set(n);
            }
            for _ in 0..4 {
                let len = if rand(4) == 0 { rand(24) } else { rand(9) };
                let s: Vec<u16> = (0..len).map(|_| b"abcA1 \n_"[rand(8)] as u16).collect();
                for start in 0..=s.len() {
                    let want = re.exec_reference(&s, start);
                    matched += want.is_some() as usize;
                    tried += 1;
                    assert_eq!(re.exec(&s, start), want, "/{p}/{f} on {:?} from {start}", String::from_utf16_lossy(&s));
                }
            }
        }
        // Both ways, often; native code, often.
        assert!(matched > tried / 4 && matched < tried * 3 / 4, "{matched} of {tried}");
        assert!(native > 5000, "{native} compiled");
    }

    /// Native code leaves an attempt to the interpreter when its stack
    /// fills, or when a unit past ASCII needs case folding.
    #[test]
    fn native_code_bails_to_the_interpreter() {
        let native = |p: &str, f: &str| {
            let re = Regex::new(p, f).unwrap();
            let n = Native::compile(&re);
            assert!(n.is_some(), "/{p}/ compiles");
            let _ = re.native.set(n);
            re
        };
        let s: Vec<u16> = "ab".repeat(1 << 20).encode_utf16().collect();
        let all = Some((0, s.len()));
        assert_eq!(native("^(?:a|b)*$", "").exec(&s, 0).unwrap()[0], all);
        let c = native("^(ab)+$", "").exec(&s, 0).unwrap();
        assert_eq!((c[0], c[1]), (all, Some((s.len() - 2, s.len()))));
        let u = |s: &str| s.encode_utf16().collect::<Vec<u16>>();
        assert_eq!(native("xé+y", "i").exec(&u("_XÉéY"), 0).unwrap()[0], Some((1, 5)));
        assert_eq!(native("[à-ï]+", "i").exec(&u("aÀÉé"), 0).unwrap()[0], Some((1, 4)));
        assert_eq!(native("[à-ï]+", "").exec(&u("aÀÉé"), 0).unwrap()[0], Some((3, 4)));
        assert_eq!(native("\\s+", "").exec(&u("a\u{a0}\u{2028} b"), 0).unwrap()[0], Some((1, 4)));
    }

    /// `find` (eight positions at a time) finds what a scan of every
    /// position does: needles of 1 to 5 units, from every start, matches
    /// near the ends included.
    #[test]
    fn find_agrees_with_a_scan() {
        let mut seed = 0x9e37_79b9_7f4a_7c15u64;
        let mut rand = move |n: usize| {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            (seed % n as u64) as usize
        };
        for _ in 0..3000 {
            let s: Vec<u16> = (0..rand(40)).map(|_| b"aab\xe9"[rand(4)] as u16).collect();
            let needle: Vec<u16> = (0..1 + rand(5)).map(|_| b"aab\xe9"[rand(4)] as u16).collect();
            for from in 0..=s.len() + 1 {
                let want = (from..s.len().saturating_sub(needle.len() - 1)).find(|&i| s[i..i + needle.len()] == *needle);
                assert_eq!(super::find(&s, &needle, from), want, "{s:?} {needle:?} from {from}");
            }
        }
    }

    /// Where `exec` starts trying (`Regex::starts`) doesn't change what it
    /// finds.
    #[test]
    fn skipping_to_where_a_match_can_start() {
        let at = |p: &str, f: &str, s: &str, start: usize| {
            let re = Regex::new(p, f).unwrap();
            let u: Vec<u16> = s.encode_utf16().collect();
            re.exec(&u, start).map(|c| c[0].unwrap())
        };
        assert!(matches!(Regex::new("^a|^b", "").unwrap().starts, Starts::Anchored));
        // A literal every match contains: tried only up to where the part
        // before it can reach.
        let nd = Regex::new("(^|[^x])ab+cde", "").unwrap().needle.unwrap();
        assert_eq!((nd.units.len(), nd.min, nd.max), (3, 2, None));
        let nd = Regex::new("[ab]?\\d{2,3}xyz", "").unwrap().needle.unwrap();
        assert_eq!((nd.units.len(), nd.min, nd.max), (3, 2, Some(4)));
        assert!(Regex::new("abc", "i").unwrap().needle.is_none());
        assert!(Regex::new("a|bc", "").unwrap().needle.is_none());
        assert_eq!(at("(^|[^x])ab", "", "xab_ab", 0), Some((3, 6)));
        assert_eq!(at("(^|[^x])ab", "", "abxab", 0), Some((0, 2)));
        assert_eq!(at("(^|[^x])ab", "", "xabxab", 0), None);
        assert_eq!(at("\\d+px", "", "1p 22px 3px", 0), Some((3, 7)));
        assert_eq!(at("\\d+px", "", "1p 22px 3px", 6), Some((8, 11)));
        assert_eq!(at("\\d{2}px", "", "123456px", 0), Some((4, 8)));
        assert_eq!(at("(a)\\1bc", "", "aabc", 0), Some((0, 4)));
        assert_eq!(at("x.*yz", "", "x1yzyz", 1), None);
        assert_eq!(at("ab", "", "a", 0), None);
        assert!(matches!(Regex::new("a*", "").unwrap().starts, Starts::Anywhere));
        assert!(matches!(Regex::new("(a)\\1", "").unwrap().starts, Starts::Units { .. }));
        assert!(matches!(Regex::new("(a)|\\1b", "").unwrap().starts, Starts::Anywhere));
        assert!(matches!(
            Regex::new("[xy]z", "").unwrap().starts,
            Starts::Units { other: false, few: Some((_, 2)), .. }
        ));
        assert!(matches!(Regex::new("[a-e]z", "").unwrap().starts, Starts::Units { few: None, .. }));
        // Folding case: an ASCII unit or class begins no match past ASCII;
        // a needle of units that fold to none but themselves.
        assert!(matches!(
            Regex::new("#|[x-z]", "i").unwrap().starts,
            Starts::Units { other: false, few: None, .. }
        ));
        let nd = Regex::new("\\s?;\\s?", "").unwrap().needle.unwrap();
        assert_eq!((nd.units.len(), nd.min, nd.max), (1, 0, Some(1)));
        assert!(Regex::new("x?;", "").unwrap().needle.is_none());
        assert_eq!(at("\\s?;\\s?", "", "a ;b; c", 3), Some((4, 6)));
        let nd = Regex::new("##yv16##", "i").unwrap().needle.unwrap();
        assert_eq!((nd.units.len(), nd.min, nd.max), (4, 4, Some(4)));
        assert!(Regex::new("ab", "i").unwrap().needle.is_none());
        assert_eq!(at("##yv16##", "i", "##yv16#x##YV16##", 0), Some((8, 16)));
        assert_eq!(at("[#é]", "i", "aÉ#", 0), Some((1, 2)));
        assert_eq!(at("[xy]z", "", "xxyyyyyyyyyyyyyyyyyyyyzz", 0), Some((21, 23)));
        // `^`: at 0 only, or after a line terminator with `m`.
        assert_eq!(at("^b", "", "ab", 0), None);
        assert_eq!(at("^a", "", "aa", 1), None);
        assert_eq!(at("^b", "m", "a\nb\rb", 1), Some((2, 3)));
        assert_eq!(at("^b", "m", "a\nb\rb", 3), Some((4, 5)));
        assert_eq!(at("^b", "m", "a\nc", 0), None);
        assert_eq!(at("^$", "m", "a\n", 0), Some((2, 2)));
        // The units a match begins with.
        assert_eq!(at("[xy]z", "", "axzyz", 2), Some((3, 5)));
        assert_eq!(at("b", "i", "aB", 0), Some((1, 2)));
        assert_eq!(at("[^a]", "", "aa\u{e9}", 0), Some((2, 3)));
        assert_eq!(at("\u{e9}", "i", "a\u{c9}", 0), Some((1, 2)));
        assert_eq!(at("[a-z]+", "i", "12XY", 0), Some((2, 4)));
        assert_eq!(at("(?=b)\\w", "", "ab", 0), Some((1, 2)));
        assert_eq!(at("x?y", "", "aay", 0), Some((2, 3)));
        assert_eq!(at("(a|)b", "", "cb", 0), Some((1, 2)));
        assert_eq!(at("\\bc", "", "a c", 0), Some((2, 3)));
        assert_eq!(at("q", "", "abc", 0), None);
        // Matches that can be empty: anywhere, the end included.
        assert_eq!(at("x*", "", "ab", 1), Some((1, 1)));
        assert_eq!(at("$", "", "ab", 0), Some((2, 2)));
    }
}
