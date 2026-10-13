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
    RegExp(String, String),
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

/// The reserved words, as [`keyword`] matches them.
#[cfg(test)]
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

pub fn lex(src: &str) -> Result<Vec<Token>, Error> {
    let b = src.as_bytes();
    let mut toks = Vec::new();
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
                Tok::RegExp(pattern, flags)
            } else if is_ascii_ident_part(c) || c >= 0x80 && src[i..].chars().next().is_some_and(is_ident_start) {
                // ASCII a byte at a time, past it a character at a time.
                let mut n = 1 + b[i + 1..].iter().take_while(|&&x| is_ascii_ident_part(x)).count();
                if b.get(i + n).is_some_and(|&x| x >= 0x80) || c >= 0x80 {
                    n = src[i..]
                        .char_indices()
                        .find(|&(_, ch)| !is_ident_part(ch))
                        .map_or(src.len() - i, |(n, _)| n);
                }
                let word = &src[i..i + n];
                i += n;
                // Keywords: lowercase, of 2 to 10 letters.
                let keyword = if c.is_ascii_lowercase() && (2..=10).contains(&n) {
                    keyword(word)
                } else {
                    None
                };
                match keyword {
                    Some(k) => Tok::Keyword(k),
                    None => Tok::Ident(names.entry(word).or_insert_with(|| word.into()).clone()),
                }
            } else if let Some(p) = PUNCTS.iter().find(|p| p.as_bytes()[0] == c && src[i..].starts_with(**p)) {
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

/// `word` as the [`KEYWORDS`] entry it is, if any (a `match`: by length,
/// then the bytes, not a scan).
fn keyword(word: &str) -> Option<&'static str> {
    Some(match word {
        "break" => "break",
        "case" => "case",
        "catch" => "catch",
        "class" => "class",
        "const" => "const",
        "continue" => "continue",
        "debugger" => "debugger",
        "default" => "default",
        "delete" => "delete",
        "do" => "do",
        "else" => "else",
        "export" => "export",
        "extends" => "extends",
        "false" => "false",
        "finally" => "finally",
        "for" => "for",
        "function" => "function",
        "if" => "if",
        "import" => "import",
        "in" => "in",
        "instanceof" => "instanceof",
        "let" => "let",
        "new" => "new",
        "null" => "null",
        "return" => "return",
        "super" => "super",
        "switch" => "switch",
        "this" => "this",
        "throw" => "throw",
        "true" => "true",
        "try" => "try",
        "typeof" => "typeof",
        "var" => "var",
        "void" => "void",
        "while" => "while",
        "with" => "with",
        _ => return None,
    })
}

fn is_ident_start(c: char) -> bool {
    c.is_alphabetic() || c == '_' || c == '$'
}

/// An ASCII identifier character (a digit only past the first: digits
/// begin numbers, lexed before identifiers).
fn is_ascii_ident_part(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$'
}

fn is_ident_part(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
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
    fn keywords_are_found_by_match() {
        for &k in KEYWORDS {
            assert_eq!(keyword(k), Some(k));
        }
        assert_eq!(keyword("iff"), None);
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
