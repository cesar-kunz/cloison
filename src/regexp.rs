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
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    /// Where a match can start (see [`Starts`]).
    starts: Starts,
    /// A literal every match contains, if any (see [`Needle`]).
    needle: Option<Needle>,
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
    /// past ASCII if `other`; `one` if that is a single unit.
    Units { ascii: u128, other: bool, one: Option<u16> },
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
    /// `^`, `$`.
    Start,
    End,
    /// `\b` (true) or `\B`.
    Boundary(bool),
    /// A group: capturing (its index) or not.
    Group(Box<Node>, Option<usize>),
    /// `(?=…)` (true) or `(?!…)`.
    Look(Box<Node>, bool),
    BackRef(usize),
    Alt(Vec<Node>),
    Seq(Vec<Node>),
    /// A repetition: minimum, maximum (`u32::MAX`: none), greedy, and the
    /// capture groups inside it (cleared at each iteration).
    Repeat(Box<Node>, u32, u32, bool, std::ops::Range<usize>),
}

impl Regex {
    /// Compile `pattern` with `flags` (`g`, `i`, `m`; `y`, `u`, `s` aren't
    /// supported).
    pub fn new(pattern: &str, flags: &str) -> Result<Regex, String> {
        let (mut global, mut ignore_case, mut multiline) = (false, false, false);
        for f in flags.chars() {
            let seen = match f {
                'g' => std::mem::replace(&mut global, true),
                'i' => std::mem::replace(&mut ignore_case, true),
                'm' => std::mem::replace(&mut multiline, true),
                _ => return Err(format!("Invalid flags supplied to RegExp constructor '{flags}'")),
            };
            if seen {
                return Err(format!("Invalid flags supplied to RegExp constructor '{flags}'"));
            }
        }
        let chars: Vec<char> = pattern.chars().collect();
        let total = count_groups(&chars);
        let mut p = Parser {
            c: &chars,
            i: 0,
            groups: 1,
            total,
        };
        let node = p.alternation()?;
        if p.i != chars.len() {
            return Err(format!("Invalid regular expression: /{pattern}/: Unmatched ')'"));
        }
        let mut re = Regex {
            node,
            groups: p.groups,
            global,
            ignore_case,
            multiline,
            starts: Starts::Anywhere,
            needle: None,
            prog: Program::default(),
            run: Default::default(),
        };
        re.prog = Program::compile(&re);
        re.starts = re.starts();
        re.needle = re.needle();
        Ok(re)
    }

    /// The longest run of two or more characters in the top-level
    /// sequence (case-sensitive patterns only).
    fn needle(&self) -> Option<Needle> {
        let Node::Seq(ns) = &self.node else {
            return None;
        };
        if self.ignore_case {
            return None;
        }
        let mut best: Option<(usize, usize)> = None;
        let mut k = 0;
        while k < ns.len() {
            let run = ns[k..].iter().take_while(|n| matches!(n, Node::Char(_))).count();
            if run >= 2 && best.is_none_or(|(_, len)| run > len) {
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
                one: (ascii.count_ones() == 1 && !other).then(|| ascii.trailing_zeros() as u16),
            },
            _ => Starts::Anywhere,
        }
    }

    /// The first match in `s` at or after `start`: each group's span.
    pub fn exec(&self, s: &[u16], start: usize) -> Option<Captures> {
        let mut kept = self.run.try_borrow_mut().ok();
        let mut fresh = Run::default();
        let run = kept.as_deref_mut().unwrap_or(&mut fresh);
        run.caps.resize(self.groups, None);
        run.regs.resize(self.prog.nregs, 0);
        self.search(s, start, |at| self.prog.run(self, s, at, run))?;
        Some(run.caps.clone())
    }

    /// Call `attempt` at each position from `start` where a match can
    /// begin, until it finds one: where.
    fn search(&self, s: &[u16], start: usize, mut attempt: impl FnMut(usize) -> bool) -> Option<usize> {
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
                Starts::Units { one: Some(c), .. } => {
                    at += s[at..].iter().position(|&x| x == c)?;
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
        let ic = self.ignore_case;
        let units = |one: &dyn Fn(u16) -> bool, other: bool| {
            let ascii = (0..128u16).filter(|&c| one(c)).fold(0u128, |m, c| m | 1 << c);
            Some((ascii, other, false))
        };
        match n {
            Node::Empty | Node::Start | Node::End | Node::Boundary(_) | Node::Look(..) => {
                Some((0, false, true))
            }
            Node::Char(c) => units(&|x| eq(ic, x, *c), *c >= 128 || ic),
            Node::Any => units(&|x| !is_line_terminator(x), true),
            Node::Class(r, neg) => units(
                &|x| in_class(ic, r, *neg, x),
                *neg || ic || r.iter().any(|&(_, hi)| hi >= 128),
            ),
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
fn eq(ic: bool, a: u16, b: u16) -> bool {
    a == b || (ic && canonical(a) == canonical(b))
}

/// Whether `c` is in the class of `ranges` (negated if `neg`), folding
/// case if `ic`.
fn in_class(ic: bool, ranges: &[(u16, u16)], neg: bool, c: u16) -> bool {
    let has = |c: u16| ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
    let mut r = has(c);
    if !r && ic {
        let u = canonical(c);
        r = has(u)
            || ranges
                .iter()
                .any(|&(lo, hi)| (lo..=hi).len() < 512 && (lo..=hi).any(|x| canonical(x) == u));
    }
    r != neg
}

/// The first `needle` in `s` at or after `from`.
fn find(s: &[u16], needle: &[u16], from: usize) -> Option<usize> {
    let mut at = from;
    while at + needle.len() <= s.len() {
        at += s[at..=s.len() - needle.len()].iter().position(|&c| c == needle[0])?;
        if s[at..at + needle.len()] == *needle {
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
fn count_groups(c: &[char]) -> usize {
    let (mut n, mut i, mut class) = (0, 0, false);
    while i < c.len() {
        match c[i] {
            '\\' => i += 1,
            '[' => class = true,
            ']' => class = false,
            '(' if !class && c.get(i + 1) != Some(&'?') => n += 1,
            _ => {}
        }
        i += 1;
    }
    n
}

struct Parser<'a> {
    c: &'a [char],
    i: usize,
    /// The next capturing group's index.
    groups: usize,
    total: usize,
}

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
        if matches!(atom, Node::Start | Node::End | Node::Boundary(_) | Node::Look(..)) {
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
                } else {
                    let g = self.groups;
                    self.groups += 1;
                    (0, Some(g))
                };
                let inner = self.alternation()?;
                if self.peek() != Some(')') {
                    return Err(self.error("Unterminated group"));
                }
                self.i += 1;
                match kind {
                    0 => Node::Group(Box::new(inner), cap),
                    k => Node::Look(Box::new(inner), k == 1),
                }
            }
            ')' => return Err(self.error("Unmatched ')'")),
            '[' => self.class()?,
            '\\' => self.escape()?,
            '{' if self.braces_at_atom() => return Err(self.error("Nothing to repeat")),
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

    fn escape(&mut self) -> Result<Node, String> {
        let Some(e) = self.peek() else {
            return Err(self.error("\\ at end of pattern"));
        };
        self.i += 1;
        Ok(match e {
            'b' => Node::Boundary(true),
            'B' => Node::Boundary(false),
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                let (ranges, neg) = class_escape(e);
                Node::Class(ranges, neg)
            }
            '1'..='9' => {
                let s = self.i - 1;
                while self.peek().is_some_and(|c| c.is_ascii_digit()) {
                    self.i += 1;
                }
                let n: usize = self.c[s..self.i].iter().collect::<String>().parse().unwrap_or(0);
                if n <= self.total {
                    Node::BackRef(n)
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
                Node::Char(self.char_escape())
            }
        })
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
        let mut ranges: Vec<(u16, u16)> = Vec::new();
        loop {
            let Some(c) = self.peek() else {
                return Err(self.error("Unterminated character class"));
            };
            if c == ']' {
                self.i += 1;
                break;
            }
            // One end: a code unit, or a class escape (`\d`: no range).
            let lo = match self.class_atom() {
                Ok(u) => u,
                Err(set) => {
                    ranges.extend(set);
                    continue;
                }
            };
            if self.peek() == Some('-') && self.c.get(self.i + 1).is_some_and(|&c| c != ']') {
                self.i += 1;
                match self.class_atom() {
                    Ok(hi) if hi >= lo => ranges.push((lo, hi)),
                    Ok(_) => return Err(self.error("Range out of order in character class")),
                    Err(set) => {
                        ranges.push((lo, lo));
                        ranges.push((b'-' as u16, b'-' as u16));
                        ranges.extend(set);
                    }
                }
            } else {
                ranges.push((lo, lo));
            }
        }
        ranges.sort();
        Ok(Node::Class(ranges, neg))
    }

    /// A class member: a code unit, or (`Err`) a class escape's ranges.
    fn class_atom(&mut self) -> Result<u16, Vec<(u16, u16)>> {
        let c = self.c[self.i];
        self.i += 1;
        if c != '\\' {
            let mut b = [0u16; 2];
            return Ok(c.encode_utf16(&mut b)[0]);
        }
        let Some(e) = self.peek() else { return Ok(b'\\' as u16) };
        match e {
            'd' | 'D' | 'w' | 'W' | 's' | 'S' => {
                self.i += 1;
                let (r, neg) = class_escape(e);
                Err(if neg { complement(&r) } else { r })
            }
            'b' => {
                self.i += 1;
                Ok(8)
            }
            '-' => {
                self.i += 1;
                Ok(b'-' as u16)
            }
            _ => Ok(self.char_escape()),
        }
    }
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
    /// One code unit, case folded: as given, or with its canonical form.
    CharFold { c: u16, canon: u16 },
    /// `.`: anything but a line terminator.
    Any,
    /// A class: ASCII members by `ascii`'s bits, the rest by
    /// `Program::classes[k]`.
    Class { ascii: u128, k: u32 },
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
    /// A lookahead, whose body follows and ends in `LookEnd`; on at
    /// `after`.
    Look { negative: bool, after: u32 },
    LookEnd,
    Match,
}

/// A compiled pattern.
#[derive(Default)]
struct Program {
    code: Vec<Insn>,
    /// Classes' ranges and negation, for units past ASCII.
    classes: Vec<(Vec<(u16, u16)>, bool)>,
    /// Registers: positions and repetition counts.
    nregs: usize,
    /// Whether case is folded (`i`).
    ic: bool,
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
    Run { pc: u32, base: usize, n: usize, min: usize },
    /// A lookahead's body runs: if it fails, continue at `after` from
    /// `pos` when `negative`; `caps` are the captures before it.
    Look { pos: usize, after: u32, negative: bool, caps: Captures },
    /// The captures before a positive lookahead that matched.
    Caps(Captures),
}

/// The interpreter's state, kept over an `exec`'s attempts.
#[derive(Default)]
struct Run {
    caps: Captures,
    regs: Vec<usize>,
    stack: Vec<Bt>,
}

impl Program {
    fn compile(re: &Regex) -> Program {
        let mut p = Program { ic: re.ignore_case, ..Program::default() };
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
        let ic = re.ignore_case;
        Some(match n {
            Node::Char(c) if ic => Insn::CharFold { c: *c, canon: canonical(*c) },
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
            self.code.push(i);
            return;
        }
        match n {
            Node::Empty => {}
            Node::Char(_) | Node::Any | Node::Class(..) => unreachable!(),
            Node::Start => self.code.push(Insn::Start),
            Node::End => self.code.push(Insn::End),
            Node::Boundary(b) => self.code.push(Insn::Boundary(*b)),
            Node::BackRef(g) => self.code.push(Insn::BackRef(*g as u32)),
            Node::Group(inner, None) => self.emit(re, inner),
            Node::Group(inner, Some(g)) => {
                let r = self.reg();
                self.code.push(Insn::Mark(r));
                self.emit(re, inner);
                self.code.push(Insn::Save { g: *g as u32, r });
            }
            Node::Look(inner, positive) => {
                let at = self.code.len();
                self.code.push(Insn::Look { negative: !positive, after: 0 });
                self.emit(re, inner);
                self.code.push(Insn::LookEnd);
                let after = self.here();
                self.code[at] = Insn::Look { negative: !positive, after };
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
            Node::Seq(nodes) => {
                for n in nodes {
                    self.emit(re, n);
                }
            }
            Node::Repeat(inner, min, max, greedy, groups) => {
                if *greedy && let Some(i) = self.unit(re, inner) {
                    self.code.push(Insn::Run { min: *min, max: *max });
                    self.code.push(i);
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
    fn unit_matches(&self, i: Insn, c: u16) -> bool {
        match i {
            Insn::Char(x) => c == x,
            Insn::CharFold { c: x, canon } => c == x || canonical(c) == canon,
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
    fn run(&self, re: &Regex, s: &[u16], start: usize, run: &mut Run) -> bool {
        let Run { caps, regs, stack } = run;
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
                    let a = pos > 0 && is_word(s[pos - 1]);
                    let b = pos < s.len() && is_word(s[pos]);
                    (a != b) == want
                }
                Insn::BackRef(g) => {
                    pc += 1;
                    match caps[g as usize] {
                        None => true,
                        Some((a, b)) => {
                            let n = b - a;
                            let ok = pos + n <= s.len() && (0..n).all(|d| eq(re.ignore_case, s[pos + d], s[a + d]));
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
                            stack.push(Bt::Run { pc: pc as u32, base: pos, n, min });
                        }
                        pos += n;
                        true
                    }
                }
                Insn::Look { negative, after } => {
                    stack.push(Bt::Look { pos, after, negative, caps: caps.clone() });
                    pc += 1;
                    true
                }
                Insn::LookEnd => {
                    // The body matched: its choice points go (a lookahead
                    // matches once), its captures stay.
                    let (at, after, negative, saved) = loop {
                        match stack.pop() {
                            Some(Bt::Look { pos, after, negative, caps }) => break (pos, after, negative, caps),
                            Some(_) => {}
                            None => unreachable!("a lookahead's end without its start"),
                        }
                    };
                    if negative {
                        *caps = saved;
                        false
                    } else {
                        stack.push(Bt::Caps(saved));
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
                    Some(Bt::Run { pc: to, base, n, min }) => {
                        let n = n - 1;
                        if n > min {
                            stack.push(Bt::Run { pc: to, base, n, min });
                        }
                        (pc, pos) = (to as usize, base + n);
                        break;
                    }
                    // The body failed.
                    Some(Bt::Look { pos: at, after, negative, .. }) => {
                        if negative {
                            (pc, pos) = (after as usize, at);
                            break;
                        }
                    }
                    Some(Bt::Caps(saved)) => *caps = saved,
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
            self.search(s, start, |at| {
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
        eq(self.re.ignore_case, a, b)
    }

    fn in_class(&self, ranges: &[(u16, u16)], neg: bool, c: u16) -> bool {
        in_class(self.re.ignore_case, ranges, neg, c)
    }

    fn node(&mut self, n: &Node, i: usize, k: K<'_, 'a>) -> bool {
        let s = self.s;
        match n {
            Node::Empty => k(self, i),
            Node::Char(c) => i < s.len() && self.eq(s[i], *c) && k(self, i + 1),
            Node::Any => i < s.len() && !is_line_terminator(s[i]) && k(self, i + 1),
            Node::Class(r, neg) => i < s.len() && self.in_class(r, *neg, s[i]) && k(self, i + 1),
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
            Node::Look(inner, positive) => {
                let saved = self.caps.clone();
                let found = self.node(inner, i, &mut |_, _| true);
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
        let (mut tried, mut matched) = (0, 0);
        for _ in 0..20_000 {
            let p = pattern(&mut rand, 3);
            let f = ["", "i", "m", "im"][rand(4)];
            let Ok(re) = Regex::new(&p, f) else { continue };
            for _ in 0..4 {
                let s: Vec<u16> = (0..rand(9)).map(|_| b"abcA1 \n_"[rand(8)] as u16).collect();
                for start in 0..=s.len() {
                    let want = re.exec_reference(&s, start);
                    matched += want.is_some() as usize;
                    tried += 1;
                    assert_eq!(re.exec(&s, start), want, "/{p}/{f} on {:?} from {start}", String::from_utf16_lossy(&s));
                }
            }
        }
        // Both ways, often.
        assert!(matched > tried / 4 && matched < tried * 3 / 4, "{matched} of {tried}");
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
            Starts::Units { other: false, one: None, .. }
        ));
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
