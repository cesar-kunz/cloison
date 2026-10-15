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
    Str(String),
    /// An operator or punctuator.
    Punct(&'static str),
    /// A regular expression literal: its pattern and flags.
    RegExp(Box<(String, String)>),
    Eof,
}

#[derive(Clone, Debug)]
pub struct Token {
    pub tok: Tok,
    /// Byte offset of its first character.
    pub pos: u32,
    /// Whether a line break precedes it (for automatic semicolons).
    pub newline_before: bool,
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
    "let",
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
            b'/' if b.get(i + 1) == Some(&b'/') => {
                while i < b.len() && b[i] != b'\n' && b[i] != b'\r' {
                    i += 1;
                }
                continue;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                let end = src[i + 2..]
                    .find("*/")
                    .ok_or_else(|| err("unterminated comment"))?;
                newline |= src[i + 2..i + 2 + end].contains(['\n', '\r']);
                i += end + 4;
                continue;
            }
            _ => {}
        }
        let tok =
            if c.is_ascii_digit() || (c == b'.' && b.get(i + 1).is_some_and(u8::is_ascii_digit)) {
                let (v, n) = number(&src[i..]).ok_or_else(|| err("bad number"))?;
                i += n;
                Tok::Num(v)
            } else if c == b'"' || c == b'\'' {
                let (s, n) = string(&src[i..]).ok_or_else(|| err("unterminated string"))?;
                i += n;
                Tok::Str(s)
            } else if c == b'`' {
                return Err(err("template literals are not supported"));
            } else if c == b'/' && !toks.last().is_some_and(|t: &Token| ends_expression(&t.tok)) {
                let (pattern, flags, n) = regexp(&src[i..]).ok_or_else(|| err("unterminated regular expression"))?;
                i += n;
                Tok::RegExp(Box::new((pattern, flags)))
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
                    match keyword {
                        Some(k) => Tok::Keyword(k),
                        None => Tok::Ident(names.entry(word).or_insert_with(|| word.into()).clone()),
                    }
                }
            } else if c == b'\\' && b.get(i + 1) == Some(&b'u') {
                let (word, m) = escaped_ident(&src[i..]).ok_or_else(|| err("bad escape in identifier"))?;
                i += m;
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
                i += p.len();
                Tok::Punct(p)
            } else {
                let ch = src[i..].chars().next().unwrap();
                return Err(err(&format!("unexpected character `{ch}`")));
            };
        toks.push(Token {
            tok,
            pos: start as u32,
            newline_before: std::mem::take(&mut newline),
        });
    }
    toks.push(Token {
        tok: Tok::Eof,
        pos: src.len() as u32,
        newline_before: true,
    });
    Ok(toks)
}

/// Whether a `/` after `t` divides (`t` ends an expression).
fn ends_expression(t: &Tok) -> bool {
    match t {
        Tok::Ident(_) | Tok::Num(_) | Tok::Str(_) | Tok::RegExp(..) => true,
        Tok::Keyword(k) => matches!(*k, "this" | "true" | "false" | "null" | "super"),
        Tok::Punct(p) => matches!(*p, ")" | "]" | "}" | "++" | "--"),
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
                if e == '\n' || e == '\r' {
                    return None;
                }
            }
            '\n' | '\r' => return None,
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
            .find(|c: char| !c.is_digit(radix))
            .unwrap_or(s.len() - 2);
        let v = u64::from_str_radix(&s[2..n], radix).ok()? as f64;
        return (!s[n..].starts_with(is_ident_part)).then_some((v, n));
    }
    let mut n = 0;
    let digits = |n: &mut usize| {
        while *n < b.len() && b[*n].is_ascii_digit() {
            *n += 1;
        }
    };
    digits(&mut n);
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
    Some((s[..n].parse().ok()?, n))
}

/// A string literal at the start of `s`: its value and length.
fn string(s: &str) -> Option<(String, usize)> {
    let quote = s.chars().next()?;
    let mut out = String::new();
    let mut it = s.char_indices().skip(1);
    while let Some((i, c)) = it.next() {
        match c {
            _ if c == quote => return Some((out, i + 1)),
            '\n' | '\r' => return None,
            '\\' => {
                let (_, e) = it.next()?;
                match e {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    'b' => out.push('\u{8}'),
                    'f' => out.push('\u{c}'),
                    'v' => out.push('\u{b}'),
                    '0' => out.push('\0'),
                    'x' | 'u' => {
                        let n = if e == 'x' { 2 } else { 4 };
                        let mut v = 0;
                        for _ in 0..n {
                            v = v * 16 + it.next()?.1.to_digit(16)?;
                        }
                        out.push(char::from_u32(v)?);
                    }
                    // A line continuation.
                    '\n' => {}
                    e => out.push(e),
                }
            }
            c => out.push(c),
        }
    }
    None
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
