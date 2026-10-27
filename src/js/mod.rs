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

/// Whether `src` is a script of one function expression alone, as what
/// `Function` makes of its parameters and body must be: text of either
/// can't end the function and go on after it (`"}); f(); (function(){"`).
pub fn is_one_function(src: &str) -> bool {
    let arena = ast::Arenas::default();
    parse::parse_source(src, &arena, true).is_ok_and(|p| {
        matches!(&p.functions[0].body[..], [ast::Stmt { kind: ast::StmtKind::Expr(e), .. }]
            if matches!(e.kind, ast::ExprKind::Function(_)))
    })
}

/// Compile `src`, a script, to a module; errors are `line:column: message`.
pub fn compile(src: &str) -> Result<Module, String> {
    let arena = ast::Arenas::default();
    parse::parse_source(src, &arena, false)
        .and_then(|p| codegen::generate(&p, src))
        .map_err(|e| e.display(src))
}

/// Module `src`'s requests, imports and exports (module `id`'s); errors
/// are `line:column: message`.
pub fn module_info(src: &str, id: i32) -> Result<ast::ModuleInfo, String> {
    let arena = ast::Arenas::default();
    let p = parse::parse_module(src, &arena, id).map_err(|e| e.display(src))?;
    Ok(p.module.unwrap_or_default())
}

/// Compile module `src` (module `id`, see `module`): a script whose value
/// is the module's function (see `ast::MODULE_FUNCTION`). `links`: per
/// import (see `ast::ModuleInfo::imports`), the variable of the scope
/// object its holder is, or none (the holder is the value).
pub fn compile_module(src: &str, id: i32, links: &[Option<String>]) -> Result<Module, String> {
    let arena = ast::Arenas::default();
    parse::parse_module(src, &arena, id)
        .and_then(|mut p| {
            for imp in p.imports.values_mut() {
                imp.prop = links[imp.index].clone();
            }
            codegen::generate(&p, src)
        })
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
        parse::parse_source(src, &arena, skim)
            .and_then(|p| codegen::generate_lazy(&p, 0, &source, &|pos| pos, &[]))
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| e.display(src))
}

/// Compile `src` as indirect `eval`'s code: as a script's, but strict
/// code's `var`s and functions its own.
pub fn compile_indirect_eval(src: &str) -> Result<Module, String> {
    let source: Rc<str> = src.into();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source(src, &arena, skim).and_then(|mut p| {
            p.eval_locals = p.functions[0].strict;
            codegen::generate_lazy(&p, 0, &source, &|pos| pos, &[])
        })
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| e.display(src))
}

/// [`compile_indirect_eval`] of strict code: a strict script's own direct
/// `eval`'s (as an indirect one's, but strict).
pub fn compile_strict_eval(src: &str) -> Result<Module, String> {
    parse::STRICT.set(true);
    let r = compile_indirect_eval(src);
    parse::STRICT.set(false);
    r
}

/// Compile `src` as direct `eval`'s code (`strict`: the caller is), in
/// `withs` `with` statements: its names looked up in their objects, then
/// in the caller's scope object (see `intrinsics::direct_eval`).
pub fn compile_direct_eval(src: &str, strict: bool, withs: usize) -> Result<Module, String> {
    let source: Rc<str> = src.into();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source_as(src, &arena, skim, Some((strict, withs)))
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
    let strict = if f.strict { "\"use strict\";" } else { "" };
    let mut text = format!("(function({}){{{strict}(function ", f.upvalues.join(","));
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
        parse::parse_source(&text, &arena, skim).and_then(|mut p| {
            p.own_names = f.own_names.iter().map(|n| (**n).into()).collect();
            codegen::generate_lazy(&p, 2, &f.source, &at, &f.upvalues)
        })
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| Error::new(at(e.pos as ast::Pos) as usize, e.msg).display(&f.source))
}

/// `String(v)` for a number, as a property key.
pub(crate) fn number_to_string(v: f64) -> String {
    crate::vm::number_to_string(v)
}
