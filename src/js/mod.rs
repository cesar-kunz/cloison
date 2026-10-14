//! The JS front end: source text into a bytecode [`Module`].
//!
//! [`lex`] turns the whole source into tokens, [`parse`] builds the syntax
//! tree of every function, and [`codegen`] compiles each function to
//! bytecode. The language is the ES5 subset the microbenchmarks in
//! `bench/micro/` are written in; the code generator further rejects what
//! the instruction set can't express yet.

pub mod ast;
pub mod codegen;
pub mod lex;
pub mod parse;

use std::rc::Rc;

use crate::asm::{LazyFn, Module};

/// An error at a byte offset of the source.
#[derive(Debug)]
pub struct Error {
    pub pos: usize,
    pub msg: String,
}

impl Error {
    pub fn new(pos: usize, msg: String) -> Error {
        Error { pos, msg }
    }

    /// `line:column: message`, both 1-based.
    pub fn display(&self, src: &str) -> String {
        let before = &src[..self.pos.min(src.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().unwrap().chars().count() + 1;
        format!("{line}:{col}: {}", self.msg)
    }
}

/// Parse `src`, a script: its syntax error, if any.
pub fn parse(src: &str) -> Result<(), Error> {
    parse::parse(&mut lex::lex(src)?, &ast::Arenas::default()).map(|_| ())
}

/// Compile `src`, a script, to a module; errors are `line:column: message`.
pub fn compile(src: &str) -> Result<Module, String> {
    let arena = ast::Arenas::default();
    lex::lex(src)
        .and_then(|mut toks| parse::parse(&mut toks, &arena))
        .and_then(|p| codegen::generate(&p))
        .map_err(|e| e.display(src))
}

/// Compile `src`, a script run by `eval` or made by `Function`, leaving
/// its functions to compile at their first calls (see
/// [`codegen::generate_lazy`]), their bodies only skimmed (see
/// [`parse::parse_with`]).
pub fn compile_script_lazy(src: &str) -> Result<Module, String> {
    let source: Rc<str> = src.into();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        lex::lex(src)
            .and_then(|mut toks| parse::parse_with(&mut toks, &arena, skim))
            .and_then(|p| codegen::generate_lazy(&p, 0, &source, &|pos| pos, &[]))
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| e.display(src))
}

/// Compile the function `f` stands for: its text, parsed again inside a
/// function declaring its upvalues (so their names resolve as where it
/// came from); the functions nested in it are left for later in turn.
/// Its code is the module's `script`.
pub fn compile_lazy(f: &LazyFn) -> Result<Module, String> {
    let mut text = format!("(function({}){{(function ", f.upvalues.join(","));
    if let Some(n) = &f.own_name {
        text.push_str(n);
    }
    let prefix = text.len();
    text.push_str(&f.source[f.start as usize..f.end as usize]);
    text.push_str(")})");
    // Offsets in `text` as in the source.
    let at = |pos: ast::Pos| (pos as usize).saturating_sub(prefix) as u32 + f.start;
    let compile = |skim| {
        let arena = ast::Arenas::default();
        lex::lex(&text)
            .and_then(|mut toks| parse::parse_with(&mut toks, &arena, skim))
            .and_then(|p| codegen::generate_lazy(&p, 2, &f.source, &at, &f.upvalues))
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| Error::new(at(e.pos as ast::Pos) as usize, e.msg).display(&f.source))
}

/// `String(v)` for a number, as a property key.
pub(crate) fn number_to_string(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e21 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
