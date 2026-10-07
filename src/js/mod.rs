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

use crate::asm::Module;

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

/// Parse `src`, a script.
pub fn parse(src: &str) -> Result<ast::Program, Error> {
    parse::parse(&lex::lex(src)?)
}

/// Compile `src`, a script, to a module; errors are `line:column: message`.
pub fn compile(src: &str) -> Result<Module, String> {
    parse(src)
        .and_then(|p| codegen::generate(&p))
        .map_err(|e| e.display(src))
}

/// `String(v)` for a number, as a property key.
pub(crate) fn number_to_string(v: f64) -> String {
    if v == v.trunc() && v.abs() < 1e21 {
        format!("{}", v as i64)
    } else {
        format!("{v}")
    }
}
