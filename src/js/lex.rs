//! The lexer: the whole source into tokens, up front.
//!
//! A `/` starts a regular expression literal where an expression can
//! start: not after a token that ends one (a name, a literal, `)`, `]`,
//! `}`, `this`, …). That's the usual rule without the parser's feedback;
//! it misreads only a regular expression right after a block's `}`.

use super::Error;
use super::ast::Name;

#[derive(Clone, Debug, PartialEq)]
pub enum Tok {
    Ident(Name),
    /// A reserved word (`function`, `var`, `this`, `true`, …).
    Keyword(&'static str),
    Num(f64),
    /// A BigInt literal: its digits (after a `0x`, `0o` or `0b`), without
    /// separators or the `n`.
    BigInt(String),
    Str(String),
    /// An operator or punctuator.
    Punct(&'static str),
    /// A regular expression literal: its pattern and flags.
    RegExp(Box<(String, String)>),
    /// A piece of a template literal: from its `` ` `` or a substitution's
    /// `}` to its `` ` `` (`tail`) or the next `${`.
    Template(Box<TemplatePart>),
    Eof,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TemplatePart {
    /// The text with its escapes decoded; `None` if one is invalid (only
    /// a tagged template allows that).
    pub cooked: Option<String>,
    /// The text as written (line breaks normalized to `\n`).
    pub raw: String,
    /// It ends the template.
    pub tail: bool,
    /// It starts the template (at its `` ` ``).
    pub head: bool,
}

/// Where lone surrogates (`"\uD800"`) go in a Rust string: code points
/// `LONE_SURROGATES + (u - 0xD800)`, in a private use plane (see
/// [`units_to_string`]; `Vm::string_constant` maps them back).
pub const LONE_SURROGATES: u32 = 0xF0000;

/// UTF-16 code units as a string, lone surrogates mapped (see
/// [`LONE_SURROGATES`]).
pub fn units_to_string(units: &[u16]) -> String {
    char::decode_utf16(units.iter().copied())
        .map(|r| match r {
            Ok(c) => c,
            Err(e) => char::from_u32(LONE_SURROGATES + (e.unpaired_surrogate() as u32 - 0xD800)).unwrap(),
        })
        .collect()
}

/// A string's UTF-16 code units, lone surrogates mapped back (see
/// [`LONE_SURROGATES`]).
pub fn string_units(s: &str, out: &mut Vec<u16>) {
    for c in s.chars() {
        let u = c as u32;
        if (LONE_SURROGATES..LONE_SURROGATES + 0x800).contains(&u) {
            out.push((u - LONE_SURROGATES + 0xD800) as u16);
        } else {
            let mut b = [0; 2];
            out.extend_from_slice(c.encode_utf16(&mut b));
        }
    }
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Byte offset of its first character.
    pub pos: u32,
    /// Whether a line break precedes it (for automatic semicolons).
    pub newline_before: bool,
    /// An identifier written with escapes (never a contextual keyword).
    pub escaped: bool,
    /// A `(` whose `)` is followed by `=>` on its line: an arrow
    /// function's parameters.
    pub arrow: bool,
}

/// The reserved words.
const KEYWORDS: &[&str] = &[
    "break",
    "case",
    "catch",
    "class",
    "const",
    "continue",
    "debugger",
    "default",
    "delete",
    "do",
    "else",
    "export",
    "extends",
    "false",
    "finally",
    "for",
    "function",
    "if",
    "import",
    "in",
    "instanceof",
    "new",
    "null",
    "return",
    "super",
    "switch",
    "this",
    "throw",
    "true",
    "try",
    "typeof",
    "var",
    "void",
    "while",
    "with",
];

/// Longest first, so that the first match is the longest.
const PUNCTS: &[&str] = &[
    ">>>=", "===", "!==", ">>>", "<<=", ">>=", "**=", "...", "&&=", "||=", "??=", "=>", "==", "!=",
    "<=", ">=", "&&", "||", "??", "?.", "++", "--", "+=", "-=", "*=", "/=", "%=", "&=", "|=", "^=",
    "<<", ">>", "**", "{", "}", "(", ")", "[", "]", ";", ",", "<", ">", "+", "-", "*", "/", "%",
    "&", "|", "^", "!", "~", "?", ":", "=", ".",
];

/// Up to 16 bytes as an integer, the first in the low byte (a word or a
/// punctuator compares as one integer, with no call to `memcmp`).
fn pack(s: &[u8]) -> u128 {
    s.iter().enumerate().fold(0, |v, (k, &c)| v | (c as u128) << (8 * k))
}

/// The punctuators beginning with byte `c`, longest first (as in
/// [`PUNCTS`]): each [`pack`]ed, with the mask of its bytes.
fn puncts(c: u8) -> &'static [(u32, u32, &'static str)] {
    static BY_FIRST: std::sync::OnceLock<Vec<Vec<(u32, u32, &'static str)>>> = std::sync::OnceLock::new();
    let by_first = BY_FIRST.get_or_init(|| {
        let mut v = vec![Vec::new(); 128];
        for &p in PUNCTS {
            let mask = (u64::MAX >> (64 - 8 * p.len())) as u32;
            v[p.as_bytes()[0] as usize].push((pack(p.as_bytes()) as u32, mask, p));
        }
        v
    });
    by_first.get(c as usize).map_or(&[], |v| v.as_slice())
}

pub fn lex(src: &str) -> Result<Vec<Token>, Error> {
    let b = src.as_bytes();
    // Minified code has a token per 2 bytes or so (jQuery's 2.2), others
    // fewer: room no token takes costs no memory, a vector grown copies.
    let mut toks = Vec::with_capacity(src.len() / 2 + 16);
    // Each distinct name once.
    let mut names: crate::jit::HashMap<&str, Name> = Default::default();
    let mut i = 0;
    let mut newline = false;
    // The `{`s open: a block's (0), an expression's (1: an object
    // literal), a template substitution's (2). After a block's `}` a `/`
    // starts a regular expression.
    let mut braces: Vec<u8> = Vec::new();
    let mut closed_expression = false;
    let mut escaped = false;
    let mut lexical = false;
    // The tokens of the `(`s open, and of the last one closed.
    let mut parens: Vec<usize> = Vec::new();
    let mut closed_paren = usize::MAX;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let err = |m: &str| Error::new(start, m.to_string());
        match c {
            b'\n' | b'\r' => {
                newline = true;
                i += 1;
                continue;
            }
            b' ' | b'\t' | 0x0b | 0x0c => {
                i += 1;
                continue;
            }
            0xC2 | 0xE1 | 0xE2 | 0xE3 | 0xEF if {
                let ch = src[i..].chars().next().unwrap();
                is_space_char(ch) || ch == '\u{2028}' || ch == '\u{2029}'
            } =>
            {
                let ch = src[i..].chars().next().unwrap();
                newline |= ch == '\u{2028}' || ch == '\u{2029}';
                i += ch.len_utf8();
                continue;
            }
            // (Annex B's HTML-like comments: `<!--`, and `-->` first on a
            // line, as `//`.)
            b'<' if b[i..].starts_with(b"<!--") => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'-' if b[i..].starts_with(b"-->") && (newline || toks.is_empty()) => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                // (To a line terminator: U+2028 and U+2029 too.)
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' && !b[i..].starts_with("\u{2028}".as_bytes()) && !b[i..].starts_with("\u{2029}".as_bytes()) {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..]
                    .find("*/")
                    .ok_or_else(|| err("unterminated comment"))?;
                newline |= src[i + 2..i + 2 + end].contains(['\n', '\r', '\u{2028}', '\u{2029}']);
                i += end + 4;
                continue;
            }
            _ => {}
        }
        let tok =
            if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
                if let Some((digits, n)) = bigint(&src[i..]) {
                    i += n;
                    Tok::BigInt(digits)
                } else {
                    let (v, n) = number(&src[i..]).ok_or_else(|| err("bad number"))?;
                    i += n;
                    Tok::Num(v)
                }
            } else if c == b'"' || c == b'\'' {
                let (s, n) = string(&src[i..]).ok_or_else(|| err("unterminated string"))?;
                // (Escapes in it: then not a directive, `"use strict"`.)
                escaped = src[i..i + n].contains('\\');
                i += n;
                Tok::Str(s)
            } else if c == b'`' || (c == b'}' && braces.last() == Some(&2)) {
                if c == b'}' {
                    braces.pop();
                }
                let (mut part, n) = template(&src[i + 1..]).ok_or_else(|| err("unterminated template literal"))?;
                part.head = c == b'`';
                i += 1 + n;
                if !part.tail {
                    braces.push(2);
                }
                Tok::Template(Box::new(part))
            } else if c == b'/'
                && !toks.last().is_some_and(|t: &Token| {
                    ends_expression(&t.tok) && (t.tok != Tok::Punct("}") || closed_expression)
                })
            {
                let (pattern, flags, n) = regexp(&src[i..]).ok_or_else(|| err("unterminated regular expression"))?;
                i += n;
                Tok::RegExp(Box::new((pattern, flags)))
            } else if c == b'#'
                && src[i + 1..].chars().next().is_some_and(|ch| ch.is_ascii_alphabetic() || ch == '_' || ch == '$' || !ch.is_ascii() && is_ident_start(ch))
            {
                // A private name (`#x`): an identifier its `#` begins,
                // never a keyword.
                let n = 1 + src[i + 1..]
                    .char_indices()
                    .find(|&(_, ch)| !is_ident_part(ch))
                    .map_or(src.len() - i - 1, |(n, _)| n);
                let word = &src[i..i + n];
                i += n;
                Tok::Ident(names.entry(word).or_insert_with(|| word.into()).clone())
            } else if is_ascii_ident_part(c) || c >= 0x80 && src[i..].chars().next().is_some_and(is_ident_start) {
                // ASCII a byte at a time, past it a character at a time.
                let mut n = 1 + b[i + 1..].iter().take_while(|&&x| is_ascii_ident_part(x)).count();
                if b.get(i + n).is_some_and(|&x| x >= 0x80) || c >= 0x80 {
                    n = src[i..]
                        .char_indices()
                        .find(|&(_, ch)| !is_ident_part(ch))
                        .map_or(src.len() - i, |(n, _)| n);
                }
                if b.get(i + n) == Some(&b'\\') {
                    // An identifier with escapes (`\u0061`): decoded,
                    // never a keyword.
                    let (word, m) = escaped_ident(&src[i..]).ok_or_else(|| err("bad escape in identifier"))?;
                    i += m;
                    escaped = true;
                    {
                    let w: &str = word.leak();
                    Tok::Ident(names.entry(w).or_insert_with(|| w.into()).clone())
                }
                } else {
                    let word = &src[i..i + n];
                    i += n;
                    // Keywords: lowercase, of 2 to 10 letters.
                    let keyword = if c.is_ascii_lowercase() && (2..=10).contains(&n) {
                        keyword(b, i - n, n)
                    } else {
                        None
                    };
                    lexical |= n == 3 && word == "let" || matches!(keyword, Some("const" | "class"));
                    match keyword {
                        Some(k) => Tok::Keyword(k),
                        None => Tok::Ident(names.entry(word).or_insert_with(|| word.into()).clone()),
                    }
                }
            } else if c == b'\\' && b.get(i + 1) == Some(&b'u') {
                let (word, m) = escaped_ident(&src[i..]).ok_or_else(|| err("bad escape in identifier"))?;
                i += m;
                escaped = true;
                {
                    let w: &str = word.leak();
                    Tok::Ident(names.entry(w).or_insert_with(|| w.into()).clone())
                }
            } else if let Some(&(_, _, p)) = {
                let next = match b.get(i..i + 4) {
                    Some(w) => u32::from_le_bytes(w.try_into().unwrap()),
                    None => pack(&b[i..]) as u32,
                };
                puncts(c).iter().find(|&&(v, mask, _)| next & mask == v)
            } {
                // (`a?.5:b` is a conditional.)
                let p = if p == "?." && b.get(i + 2).is_some_and(u8::is_ascii_digit) { "?" } else { p };
                i += p.len();
                if p == "{" {
                    let expression = match toks.last().map(|t: &Token| &t.tok) {
                        None => false,
                        // A function expression's body (`(function () {}) / 1`).
                        Some(Tok::Punct(")")) if closed_paren != usize::MAX => {
                            let mut j = closed_paren;
                            if j > 0 && matches!(toks[j - 1].tok, Tok::Ident(_)) {
                                j -= 1;
                            }
                            j > 1
                                && toks[j - 1].tok == Tok::Keyword("function")
                                && !matches!(
                                    toks[j - 2].tok,
                                    Tok::Punct(";" | "}" | "{" | ")") | Tok::Keyword("else" | "do")
                                )
                        }
                        Some(Tok::Punct(q)) => !matches!(*q, ")" | "]" | "}" | ";" | "{" | "=>"),
                        Some(Tok::Keyword(k)) => matches!(
                            *k,
                            "return" | "typeof" | "case" | "delete" | "void" | "throw" | "new" | "in" | "instanceof"
                        ),
                        Some(Tok::Template(_)) => true,
                        _ => false,
                    };
                    braces.push(expression as u8);
                } else if p == "}" {
                    closed_expression = braces.pop() == Some(1);
                } else if p == "(" {
                    parens.push(toks.len());
                } else if p == ")" {
                    closed_paren = parens.pop().unwrap_or(usize::MAX);
                } else if p == "=>"
                    && !newline
                    && toks.last().is_some_and(|t: &Token| t.tok == Tok::Punct(")"))
                    && closed_paren != usize::MAX
                {
                    toks[closed_paren].arrow = true;
                }
                Tok::Punct(p)
            } else {
                let ch = src[i..].chars().next().unwrap();
                return Err(err(&format!("unexpected character `{ch}`")));
            };
        toks.push(Token {
            tok,
            pos: start as u32,
            newline_before: std::mem::take(&mut newline),
            escaped: std::mem::take(&mut escaped),
            arrow: false,
        });
    }
    toks.push(Token {
        tok: Tok::Eof,
        pos: src.len() as u32,
        newline_before: true,
        // (On the end: whether `let`, `const` or `class` appear.)
        escaped: lexical,
        arrow: false,
    });
    Ok(toks)
}

/// Whether a `/` after `t` divides (`t` ends an expression).
fn ends_expression(t: &Tok) -> bool {
    match t {
        Tok::Ident(_) | Tok::Num(_) | Tok::BigInt(_) | Tok::Str(_) | Tok::RegExp(..) => true,
        Tok::Keyword(k) => matches!(*k, "this" | "true" | "false" | "null" | "super"),
        Tok::Punct(p) => matches!(*p, ")" | "]" | "}" | "++" | "--"),
        Tok::Template(t) => t.tail,
        Tok::Eof => false,
    }
}

/// A regular expression literal at the start of `s` (at its `/`): its
/// pattern (escapes kept for the regular expression compiler), flags and
/// length. A `/` in a class (`[/]`) doesn't end it.
fn regexp(s: &str) -> Option<(String, String, usize)> {
    let mut class = false;
    let mut it = s.char_indices().skip(1);
    let end = loop {
        let (i, c) = it.next()?;
        match c {
            '\\' => {
                let (_, e) = it.next()?;
                if matches!(e, '\n' | '\r' | '\u{2028}' | '\u{2029}') {
                    return None;
                }
            }
            '\n' | '\r' | '\u{2028}' | '\u{2029}' => return None,
            '[' => class = true,
            ']' => class = false,
            '/' if !class => break i,
            _ => {}
        }
    };
    let flags_len = s[end + 1..]
        .char_indices()
        .find(|&(_, c)| !is_ident_part(c))
        .map_or(s.len() - end - 1, |(n, _)| n);
    let flags = s[end + 1..end + 1 + flags_len].to_string();
    Some((s[1..end].to_string(), flags, end + 1 + flags_len))
}

/// The word of `n` bytes at `b[i..]` as the [`KEYWORDS`] entry it is,
/// if any: one of those of its first letter and length, compared as
/// [`pack`]ed integers.
fn keyword(b: &[u8], i: usize, n: usize) -> Option<&'static str> {
    static BY_FIRST: std::sync::OnceLock<Vec<Vec<(usize, u128, &'static str)>>> = std::sync::OnceLock::new();
    let by_first = BY_FIRST.get_or_init(|| {
        let mut v = vec![Vec::new(); 26];
        for &k in KEYWORDS {
            v[(k.as_bytes()[0] - b'a') as usize].push((k.len(), pack(k.as_bytes()), k));
        }
        v
    });
    let first = by_first.get(b[i].wrapping_sub(b'a') as usize)?;
    let w = match b.get(i..i + 16) {
        Some(w) => u128::from_le_bytes(w.try_into().unwrap()) & (u128::MAX >> (128 - 8 * n)),
        None => pack(&b[i..i + n]),
    };
    first.iter().find(|&&(len, v, _)| len == n && v == w).map(|&(.., k)| k)
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$' || is_other_id_start(c)
}

/// Unicode's Other_ID_Start and Other_ID_Continue characters, which
/// `char::is_alphanumeric` leaves out.
fn is_other_id_start(c: char) -> bool {
    matches!(c, '\u{1885}' | '\u{1886}' | '\u{2118}' | '\u{212E}' | '\u{309B}' | '\u{309C}')
}

fn is_other_id_continue(c: char) -> bool {
    matches!(c, '\u{00B7}' | '\u{0387}' | '\u{1369}'..='\u{1371}' | '\u{19DA}' | '\u{200C}' | '\u{200D}' | '\u{30FB}' | '\u{FF65}')
        || matches!(unicode_general_category(c), Some("Mn" | "Mc" | "Nd" | "Pc"))
}

/// Whether `c` is Unicode whitespace as JavaScript has it (`Zs`, tab,
/// vertical tab, form feed, no-break space, BOM).
fn is_space_char(c: char) -> bool {
    matches!(
        c,
        '\t' | '\u{0B}' | '\u{0C}' | ' ' | '\u{A0}' | '\u{1680}' | '\u{2000}'..='\u{200A}' | '\u{202F}' | '\u{205F}' | '\u{3000}' | '\u{FEFF}'
    )
}

/// A rough general category for the combining marks and connectors
/// identifiers may continue with (the ranges `char` doesn't classify).
fn unicode_general_category(c: char) -> Option<&'static str> {
    let u = c as u32;
    if matches!(u, 0x0300..=0x036F | 0x0483..=0x0487 | 0x0591..=0x05BD | 0x0610..=0x061A | 0x064B..=0x065F | 0x0670 | 0x06D6..=0x06DC | 0x0E31 | 0x0E34..=0x0E3A | 0x0E47..=0x0E4E | 0x1AB0..=0x1AFF | 0x1DC0..=0x1DFF | 0x20D0..=0x20F0 | 0xFE00..=0xFE0F | 0xFE20..=0xFE2F) {
        return Some("Mn");
    }
    if matches!(u, 0x0903 | 0x093B | 0x093E..=0x0940 | 0x0949..=0x094C | 0x094E | 0x094F) {
        return Some("Mc");
    }
    if matches!(u, 0x203F | 0x2040 | 0x2054 | 0xFE33 | 0xFE34 | 0xFE4D..=0xFE4F | 0xFF3F) {
        return Some("Pc");
    }
    if c.is_numeric() && c.to_digit(10).is_some() {
        return Some("Nd");
    }
    None
}

/// An identifier with `\uXXXX` or `\u{X…}` escapes at the start of `s`:
/// its decoded name and its length.
fn escaped_ident(s: &str) -> Option<(String, usize)> {
    let mut out = String::new();
    let b = s.as_bytes();
    let mut i = 0;
    while i < b.len() {
        let c = s[i..].chars().next()?;
        let ch = if c == '\\' {
            if b.get(i + 1) != Some(&b'u') {
                return None;
            }
            let (cp, n) = if b.get(i + 2) == Some(&b'{') {
                let end = s[i + 3..].find('}')?;
                (u32::from_str_radix(&s[i + 3..i + 3 + end], 16).ok()?, end + 4)
            } else {
                (u32::from_str_radix(s.get(i + 2..i + 6)?, 16).ok()?, 6)
            };
            i += n;
            char::from_u32(cp)?
        } else {
            i += c.len_utf8();
            c
        };
        let ok = if out.is_empty() { is_ident_start(ch) } else { is_ident_part(ch) };
        if !ok {
            if c == '\\' {
                return None;
            }
            i -= c.len_utf8();
            break;
        }
        out.push(ch);
    }
    if out.is_empty() {
        return None;
    }
    Some((out, i))
}



/// An ASCII identifier character (a digit only past the first: digits
/// begin numbers, lexed before identifiers).
fn is_ascii_ident_part(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

fn is_ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$' || is_other_id_start(c) || is_other_id_continue(c)
}

/// A numeric literal at the start of `s`: its value and length.
fn number(s: &str) -> Option<(f64, usize)> {
    let b = s.as_bytes();
    if b.len() > 1 && b[0] == b'0' && matches!(b[1], b'x' | b'X' | b'o' | b'O' | b'b' | b'B') {
        let radix = match b[1] | 0x20 {
            b'x' => 16,
            b'o' => 8,
            _ => 2,
        };
        let n = 2 + s[2..]
            .find(|c: char| !c.is_digit(radix) && c != '_')
            .unwrap_or(s.len() - 2);
        if n == 2 || !separated(&s[2..n]) {
            return None;
        }
        // (As a double: past 2^64 too.)
        let v = s[2..n]
            .chars()
            .filter(|&c| c != '_')
            .fold(0.0, |v, c| v * radix as f64 + c.to_digit(radix).unwrap() as f64);
        return (!s[n..].starts_with(is_ident_part)).then_some((v, n));
    }
    let mut n = 0;
    // (With numeric separators, `1_000`.)
    let digits = |n: &mut usize| {
        while *n < b.len() && (b[*n].is_ascii_digit() || b[*n] == b'_') {
            *n += 1;
        }
    };
    digits(&mut n);
    // (No separator in one with a leading zero.)
    if b[0] == b'0' && n > 1 && b[..n].contains(&b'_') {
        return None;
    }
    // A legacy octal integer (`070`, Annex B; not `08`, decimal).
    if b[0] == b'0' && n > 1 && b[1..n].iter().all(|d| (b'0'..=b'7').contains(d)) {
        let v = s[1..n].bytes().fold(0.0, |v, d| v * 8.0 + (d - b'0') as f64);
        return (!s[n..].starts_with(is_ident_part)).then_some((v, n));
    }
    if n < b.len() && b[n] == b'.' {
        n += 1;
        digits(&mut n);
    }
    if n < b.len() && (b[n] | 0x20) == b'e' {
        let mut m = n + 1;
        if m < b.len() && matches!(b[m], b'+' | b'-') {
            m += 1;
        }
        if m < b.len() && b[m].is_ascii_digit() {
            n = m;
            digits(&mut n);
        }
    }
    if s[n..].starts_with(is_ident_part) {
        return None;
    }
    if !s[..n].contains('_') {
        return Some((s[..n].parse().ok()?, n));
    }
    // Each separator between two digits.
    let parts_ok = s[..n].split(['.', 'e', 'E', '+', '-']).all(separated);
    if !parts_ok {
        return None;
    }
    Some((s[..n].replace('_', "").parse().ok()?, n))
}

/// A BigInt literal at the start of `s` (integer digits and `n`): its
/// digits, with their radix prefix, and its length.
fn bigint(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let (radix, start) = match b {
        [b'0', p, ..] if matches!(p | 0x20, b'x' | b'o' | b'b') => {
            (match p | 0x20 { b'x' => 16, b'o' => 8, _ => 2 }, 2)
        }
        _ => (10, 0),
    };
    let n = start + s[start..].find(|c: char| !c.is_digit(radix) && c != '_').unwrap_or(s.len() - start);
    if n == start || b.get(n) != Some(&b'n') || !separated(&s[start..n]) {
        return None;
    }
    // (No leading zero: not `01n`, legacy octal.)
    if radix == 10 && b[0] == b'0' && n > 1 {
        return None;
    }
    if s[n + 1..].starts_with(is_ident_part) {
        return None;
    }
    Some((s[..start].to_ascii_lowercase() + &s[start..n].replace('_', ""), n + 1))
}

/// Whether digits `s`' separators (`_`) are each between two digits.
fn separated(s: &str) -> bool {
    !s.starts_with('_') && !s.ends_with('_') && !s.contains("__")
}

/// A string literal at the start of `s`: its value and length.
fn string(s: &str) -> Option<(String, usize)> {
    let b = s.as_bytes();
    let quote = b[0];
    // Mostly no escapes: the text as is.
    if let Some(k) = b[1..].iter().position(|&c| c == quote || c == b'\\' || c == b'\n' || c == b'\r') {
        if b[1 + k] == quote {
            return Some((s[1..1 + k].to_string(), k + 2));
        }
    }
    let mut out: Vec<u16> = Vec::new();
    let mut i = 1;
    while i < b.len() {
        let c = b[i];
        if c == quote {
            return Some((units_to_string(&out), i + 1));
        }
        match c {
            b'\n' | b'\r' => return None,
            b'\\' => {
                let (ok, n) = escape(s, i, &mut out);
                if !ok {
                    return None;
                }
                i += n;
            }
            _ => {
                let ch = s[i..].chars().next()?;
                let mut buf = [0; 2];
                out.extend_from_slice(ch.encode_utf16(&mut buf));
                i += ch.len_utf8();
            }
        }
    }
    None
}

/// The escape sequence at `s[i..]` (at its `\`): its code units onto
/// `out`, whether it's valid, and its length (an invalid one's up to
/// where it goes wrong, at least 2).
fn escape(s: &str, i: usize, out: &mut Vec<u16>) -> (bool, usize) {
    let b = s.as_bytes();
    let Some(&e) = b.get(i + 1) else {
        return (false, 1);
    };
    let unit = |out: &mut Vec<u16>, u: u16| out.push(u);
    match e {
        b'n' => unit(out, 10),
        b't' => unit(out, 9),
        b'r' => unit(out, 13),
        b'b' => unit(out, 8),
        b'f' => unit(out, 12),
        b'v' => unit(out, 11),
        b'0'..=b'7' => {
            // `\0`, or a legacy octal escape (up to 377).
            let mut v = (e - b'0') as u32;
            let mut n = 2;
            let max = if e <= b'3' { 3 } else { 2 };
            while n < max + 1 && b.get(i + n).is_some_and(|d| (b'0'..=b'7').contains(d)) {
                v = v * 8 + (b[i + n] - b'0') as u32;
                n += 1;
            }
            unit(out, v as u16);
            return (true, n);
        }
        b'x' => {
            let v = s.get(i + 2..i + 4).and_then(|h| u32::from_str_radix(h, 16).ok().filter(|_| h.bytes().all(|c| c.is_ascii_hexdigit())));
            match v {
                Some(v) => {
                    unit(out, v as u16);
                    return (true, 4);
                }
                None => return (false, 2),
            }
        }
        b'u' => {
            if b.get(i + 2) == Some(&b'{') {
                let Some(end) = s[i + 3..].find('}') else {
                    return (false, 2);
                };
                let h = &s[i + 3..i + 3 + end];
                match u32::from_str_radix(h, 16) {
                    Ok(v) if !h.is_empty() && h.bytes().all(|c| c.is_ascii_hexdigit()) && v <= 0x10FFFF => {
                        if v >= 0x10000 {
                            let v = v - 0x10000;
                            unit(out, 0xD800 + (v >> 10) as u16);
                            unit(out, 0xDC00 + (v & 0x3FF) as u16);
                        } else {
                            unit(out, v as u16);
                        }
                        return (true, end + 4);
                    }
                    _ => return (false, 2),
                }
            }
            let v = s.get(i + 2..i + 6).filter(|h| h.bytes().all(|c| c.is_ascii_hexdigit())).and_then(|h| u32::from_str_radix(h, 16).ok());
            match v {
                Some(v) => {
                    unit(out, v as u16);
                    return (true, 6);
                }
                None => return (false, 2),
            }
        }
        // A line continuation.
        b'\r' => {
            return (true, if b.get(i + 2) == Some(&b'\n') { 3 } else { 2 });
        }
        b'\n' => {}
        _ => {
            let ch = s[i + 1..].chars().next().unwrap();
            if ch == '\u{2028}' || ch == '\u{2029}' {
                return (true, 1 + ch.len_utf8());
            }
            let mut buf = [0; 2];
            out.extend_from_slice(ch.encode_utf16(&mut buf));
            return (true, 1 + ch.len_utf8());
        }
    }
    (true, 2)
}

/// A template literal's piece at the start of `s` (past its `` ` `` or
/// `}`): the piece and its length, up to and including its `` ` `` or
/// `${`.
fn template(s: &str) -> Option<(TemplatePart, usize)> {
    let b = s.as_bytes();
    let mut cooked: Option<Vec<u16>> = Some(Vec::new());
    let mut raw = String::new();
    let mut i = 0;
    loop {
        let c = *b.get(i)?;
        match c {
            b'`' => {
                let cooked = cooked.map(|u| units_to_string(&u));
                return Some((TemplatePart { cooked, raw, tail: true, head: false }, i + 1));
            }
            b'$' if b.get(i + 1) == Some(&b'{') => {
                let cooked = cooked.map(|u| units_to_string(&u));
                return Some((TemplatePart { cooked, raw, tail: false, head: false }, i + 2));
            }
            b'\\' => {
                let mut units = Vec::new();
                let (ok, n) = escape(s, i, &mut units);
                // (`\0` followed by a digit, or an octal escape: invalid.)
                let octal = b.get(i + 1).is_some_and(|d| d.is_ascii_digit())
                    && !(b[i + 1] == b'0' && !b.get(i + 2).is_some_and(|d| d.is_ascii_digit()));
                if !ok || octal {
                    cooked = None;
                } else if let Some(c) = &mut cooked {
                    c.extend_from_slice(&units);
                }
                let n = n.max(2).min(s.len() - i);
                // (An escaped line break is raw `\` and `\n`.)
                raw.push_str(&s[i..i + n].replace("\r\n", "\n").replace('\r', "\n"));
                i += n;
            }
            b'\r' => {
                let n = if b.get(i + 1) == Some(&b'\n') { 2 } else { 1 };
                raw.push('\n');
                if let Some(c) = &mut cooked {
                    c.push(10);
                }
                i += n;
            }
            _ => {
                let ch = s[i..].chars().next()?;
                raw.push(ch);
                if let Some(c) = &mut cooked {
                    let mut buf = [0; 2];
                    c.extend_from_slice(ch.encode_utf16(&mut buf));
                }
                i += ch.len_utf8();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn toks(s: &str) -> Vec<Tok> {
        lex(s).unwrap().into_iter().map(|t| t.tok).collect()
    }

    #[test]
    fn lexes_words_numbers_strings_and_operators() {
        assert_eq!(
            toks("var x1 = 0x1F + .5e1 >>>= 'a\\n'; // c\n/* d */ y"),
            [
                Tok::Keyword("var"),
                Tok::Ident("x1".into()),
                Tok::Punct("="),
                Tok::Num(31.0),
                Tok::Punct("+"),
                Tok::Num(5.0),
                Tok::Punct(">>>="),
                Tok::Str("a\n".into()),
                Tok::Punct(";"),
                Tok::Ident("y".into()),
                Tok::Eof,
            ]
        );
    }

    /// Identifiers: ASCII, past it, and words that only look like keywords.
    #[test]
    fn lexes_identifiers_and_keywords() {
        let id = |s: &str| Tok::Ident(s.into());
        assert_eq!(
            toks("aé1 éb $x_9 _ instanceofx instanceof If in i iff"),
            [
                id("aé1"),
                id("éb"),
                id("$x_9"),
                id("_"),
                id("instanceofx"),
                Tok::Keyword("instanceof"),
                id("If"),
                Tok::Keyword("in"),
                id("i"),
                id("iff"),
                Tok::Eof,
            ]
        );
    }

    #[test]
    fn keywords_are_found() {
        let word = |w: &str| keyword(w.as_bytes(), 0, w.len());
        for &k in KEYWORDS {
            assert_eq!(word(k), Some(k));
            let padded = format!("{k}_padding_beyond_16");
            assert_eq!(keyword(padded.as_bytes(), 0, k.len()), Some(k));
        }
        assert_eq!(word("iff"), None);
        assert_eq!(word("thi"), None);
        assert_eq!(word("thisx"), None);
    }

    #[test]
    fn records_line_breaks_and_positions() {
        let t = lex("a /* \n */ b\nc d").unwrap();
        let nl: Vec<bool> = t.iter().map(|t| t.newline_before).collect();
        assert_eq!(nl, [false, true, true, false, true]);
        assert_eq!(t[3].pos, 14);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(lex("'abc").is_err());
        assert!(lex("3in").is_err());
        assert!(lex("a # b").is_err());
        assert!(lex("/* x").is_err());
    }
}
