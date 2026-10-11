//! Regular expressions: a parser for ECMAScript's pattern syntax and a
//! backtracking matcher over UTF-16 code units (as the strings are).
//!
//! The matcher passes continuations: matching a node calls `k` with the
//! position after it, and backtracks when `k` fails. Simple and complete
//! (backreferences, lookahead, lazy quantifiers), but it recurses once per
//! character a repetition matches, so very long matches use a deep native
//! stack. A compiled engine is future work (see docs/future-work.md).

/// A compiled regular expression.
pub struct Regex {
    node: Node,
    /// Capture groups, group 0 (the whole match) included.
    pub groups: usize,
    pub global: bool,
    pub ignore_case: bool,
    pub multiline: bool,
    /// Where a match can start (see [`Starts`]).
    starts: Starts,
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
    /// A repetition: minimum, maximum (`u32::MAX`: none), greedy.
    Repeat(Box<Node>, u32, u32, bool),
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
        };
        re.starts = re.starts();
        Ok(re)
    }

    fn starts(&self) -> Starts {
        if anchored(&self.node) {
            return Starts::Anchored;
        }
        let m = Matcher {
            re: self,
            s: &[],
            caps: Vec::new(),
        };
        match m.first(&self.node) {
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
        let mut m = Matcher {
            re: self,
            s,
            caps: vec![None; self.groups],
        };
        let mut at = start;
        while at <= s.len() {
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
            for c in m.caps.iter_mut() {
                *c = None;
            }
            let from = at;
            if m.node(&self.node, at, &mut |m, end| {
                m.caps[0] = Some((from, end));
                true
            }) {
                return Some(m.caps);
            }
            at += 1;
        }
        None
    }
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
            let atom = self.atom()?;
            let atom = self.quantifier(atom)?;
            seq.push(atom);
        }
        Ok(match seq.len() {
            0 => Node::Empty,
            1 => seq.pop().unwrap(),
            _ => Node::Seq(seq),
        })
    }

    fn quantifier(&mut self, atom: Node) -> Result<Node, String> {
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
        Ok(Node::Repeat(Box::new(atom), min, max, greedy))
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

struct Matcher<'a> {
    re: &'a Regex,
    s: &'a [u16],
    caps: Captures,
}

type K<'k, 'a> = &'k mut dyn FnMut(&mut Matcher<'a>, usize) -> bool;

impl<'a> Matcher<'a> {
    fn eq(&self, a: u16, b: u16) -> bool {
        a == b || (self.re.ignore_case && canonical(a) == canonical(b))
    }

    fn in_class(&self, ranges: &[(u16, u16)], neg: bool, c: u16) -> bool {
        let has = |c: u16| ranges.iter().any(|&(lo, hi)| lo <= c && c <= hi);
        let mut r = has(c);
        if !r && self.re.ignore_case {
            let u = canonical(c);
            r = has(u)
                || ranges
                    .iter()
                    .any(|&(lo, hi)| (lo..=hi).len() < 512 && (lo..=hi).any(|x| canonical(x) == u));
        }
        r != neg
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
            Node::Repeat(inner, min, max, greedy) => self.repeat(inner, *min, *max, *greedy, 0, i, k),
        }
    }

    fn seq(&mut self, nodes: &[Node], i: usize, k: K<'_, 'a>) -> bool {
        match nodes {
            [] => k(self, i),
            [n] => self.node(n, i, k),
            [n, rest @ ..] => self.node(n, i, &mut |m, j| m.seq(rest, j, k)),
        }
    }

    /// What `n`'s matches can begin with (see [`First`]): ASCII units are
    /// tested as matching does, so with its case folding (which maps no
    /// unit past ASCII to one in it, or back).
    fn first(&self, n: &Node) -> First {
        let units = |one: &dyn Fn(u16) -> bool, other: bool| {
            let ascii = (0..128u16).filter(|&c| one(c)).fold(0u128, |m, c| m | 1 << c);
            Some((ascii, other, false))
        };
        match n {
            Node::Empty | Node::Start | Node::End | Node::Boundary(_) | Node::Look(..) => {
                Some((0, false, true))
            }
            Node::Char(c) => units(&|x| self.eq(x, *c), *c >= 128 || self.re.ignore_case),
            Node::Any => units(&|x| !is_line_terminator(x), true),
            Node::Class(r, neg) => units(
                &|x| self.in_class(r, *neg, x),
                *neg || self.re.ignore_case || r.iter().any(|&(_, hi)| hi >= 128),
            ),
            Node::Group(inner, _) => self.first(inner),
            Node::BackRef(_) => None,
            Node::Repeat(inner, min, _, _) => {
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

    /// `inner` repeated `count` times so far, from `i`.
    #[allow(clippy::too_many_arguments)]
    fn repeat(
        &mut self,
        inner: &Node,
        min: u32,
        max: u32,
        greedy: bool,
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
        let more = |m: &mut Self, k: K<'_, 'a>| {
            count < max
                && m.node(inner, i, &mut |m, j| {
                    // An empty iteration past the minimum ends the loop.
                    if j == i && count >= min {
                        return false;
                    }
                    m.repeat(inner, min, max, greedy, count + 1, j, k)
                })
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
