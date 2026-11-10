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


use crate::asm::{Checked, LazyFn, Module};
use lex::Source;

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

    /// `line:column: message`, both 1-based, the offset one of `text`
    /// (a file's text, as the entry points taking a `&str` take it).
    pub fn display(&self, text: &str) -> String {
        let before = &text[..self.pos.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let col = before.rsplit('\n').next().unwrap().chars().count() + 1;
        format!("{line}:{col}: {}", self.msg)
    }

    /// [`Error::display`], the offset one of source code `src`: a
    /// string's code units' column counts their characters, not the
    /// `ESCAPE`s (see `lex::Source`).
    pub fn display_source(&self, src: &Source) -> String {
        if !src.is_units() {
            return self.display(src.as_str());
        }
        let s = src.as_str();
        let before = &s[..self.pos.min(s.len())];
        let line = before.matches('\n').count() + 1;
        let last = &before[before.rfind('\n').map_or(0, |k| k + 1)..];
        let col = last.chars().count() - lex::escapes_before(last, last.len()) + 1;
        format!("{line}:{col}: {}", self.msg)
    }
}

// The entry points taking a `&str` take a file's text, their offsets (an
// AST's, a token's, an error's) the text's; the others, source code
// (`lex::Source`), their offsets its: `eval`'s and `Function`'s a string's
// code units, lone surrogates their own (see `lex::Source::from_units`).

/// Parse `text`, a script's text: its syntax error, if any (at an offset
/// of `text`).
pub fn parse(text: &str) -> Result<(), Error> {
    check_source(&Source::from_text(text))
}

/// [`parse`] of source code `src` (its error at an offset of `src`).
pub fn check_source(src: &Source) -> Result<(), Error> {
    parse::parse(&mut lex::lex_source(src, false)?, &ast::Arenas::default()).map(|_| ())
}

/// Whether `src` is a script of one function expression alone, as what
/// `Function` makes of its parameters and body must be: text of either
/// can't end the function and go on after it (`"}); f(); (function(){"`).
pub fn is_one_function_source(src: &Source) -> bool {
    let arena = ast::Arenas::default();
    parse::parse_source_code(src, &arena, true).is_ok_and(|p| {
        matches!(&p.functions[0].body[..], [ast::Stmt { kind: ast::StmtKind::Expr(e), .. }]
            if matches!(e.kind, ast::ExprKind::Function(_)))
    })
}

/// Compile `text`, a script's text, to a module; errors are
/// `line:column: message`.
pub fn compile(text: &str) -> Result<Checked, String> {
    let src = Source::from_text(text);
    compile_checked(&src).map_err(|e| e.display(text))
}

/// [`compile`] of source code `src`.
pub fn compile_source(src: &Source) -> Result<Checked, String> {
    compile_checked(src).map_err(|e| e.display_source(src))
}

fn compile_checked(src: &Source) -> Result<Checked, Error> {
    let arena = ast::Arenas::default();
    parse::parse_source_code(src, &arena, false)
        .and_then(|p| codegen::generate_source(&p, src))
        // SAFETY: the front end's code is right by construction.
        .map(|m| unsafe { Checked::new_unchecked(m) })
}

/// Module text `text`'s requests, imports and exports (module `id`'s);
/// errors are `line:column: message`.
pub fn module_info(text: &str, id: i32) -> Result<ast::ModuleInfo, String> {
    let src = Source::from_text(text);
    let arena = ast::Arenas::default();
    let p = parse::parse_module_source(&src, &arena, id).map_err(|e| e.display(text))?;
    Ok(p.module.unwrap_or_default())
}

/// Compile module text `text` (module `id`, see `module`): a script whose
/// value is the module's function (see `ast::MODULE_FUNCTION`). `links`:
/// per import (see `ast::ModuleInfo::imports`), the variable of the scope
/// object its holder is, or none (the holder is the value).
pub fn compile_module(text: &str, id: i32, links: &[Option<String>]) -> Result<Module, String> {
    let src = Source::from_text(text);
    let arena = ast::Arenas::default();
    parse::parse_module_source(&src, &arena, id)
        .and_then(|mut p| {
            for imp in p.imports.values_mut() {
                imp.prop = links[imp.index].clone();
            }
            codegen::generate_source(&p, &src)
        })
        .map_err(|e| e.display(text))
}

/// Compile `src`, a script run by `eval` or made by `Function`, leaving
/// its functions to compile at their first calls (see
/// [`codegen::generate_lazy`]), their bodies only skimmed (see
/// [`parse::parse_with`]).
pub fn compile_script_lazy_source(src: &Source) -> Result<Module, String> {
    let (source, at) = src.unit_string();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source_code(src, &arena, skim)
            .and_then(|p| codegen::generate_lazy_units(&p, 0, &source, &*at, &[]))
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| e.display_source(src))
}

/// Compile `src` as indirect `eval`'s code: as a script's, but its
/// block-scoped bindings its own, and strict code's `var`s and functions.
pub fn compile_indirect_eval_source(src: &Source) -> Result<Module, String> {
    let (source, at) = src.unit_string();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source_code(src, &arena, skim).and_then(|mut p| {
            p.eval_locals = p.functions[0].strict;
            p.script = false;
            codegen::generate_lazy_units(&p, 0, &source, &*at, &[])
        })
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| e.display_source(src))
}

/// [`compile_indirect_eval`] of strict code: a strict script's own direct
/// `eval`'s (as an indirect one's, but strict).
pub fn compile_strict_eval_source(src: &Source) -> Result<Module, String> {
    parse::STRICT.set(true);
    let r = compile_indirect_eval_source(src);
    parse::STRICT.set(false);
    r
}

/// Compile `src` as direct `eval`'s code (`strict`: the caller is;
/// `new_target`: it has a `new.target`, see `parse::parse_source_as`;
/// `home`: a home object, a method's, see `ast::EVAL_HOME`; `live_this`:
/// its `this` the caller's binding, see `ast::EVAL_LIVE_THIS`), in the
/// scopes `chain` (see `parse::parse_pass`): its names looked up in their
/// objects (see `intrinsics::direct_eval`). Or, with `bound`
/// (`ast::Program::eval_upvalues`), also in the caller's bindings, its
/// upvalues (see `intrinsics::direct_eval_bound`).
pub fn compile_direct_eval_source(
    src: &Source,
    strict: bool,
    new_target: bool,
    home: bool,
    live_this: bool,
    chain: &str,
    global: bool,
    bound: &[(ast::Name, u8, ast::Name, usize)],
) -> Result<Module, String> {
    let (source, at) = src.unit_string();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source_code_as(src, &arena, skim, Some((strict, chain, !bound.is_empty(), global, new_target, home, live_this))).and_then(|mut p| {
            p.eval_upvalues = bound.to_vec();
            codegen::generate_lazy_units(&p, 0, &source, &*at, &[])
        })
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| e.display_source(src))
}

// The same of a script's text (`line:column`s the text's: see
// `Error::display_source`).

/// [`is_one_function_source`] of a script's text.
pub fn is_one_function(text: &str) -> bool {
    is_one_function_source(&Source::from_text(text))
}

/// [`compile_script_lazy_source`] of a script's text.
pub fn compile_script_lazy(text: &str) -> Result<Module, String> {
    compile_script_lazy_source(&Source::from_text(text))
}

/// [`compile_indirect_eval_source`] of a script's text.
pub fn compile_indirect_eval(text: &str) -> Result<Module, String> {
    compile_indirect_eval_source(&Source::from_text(text))
}

/// [`compile_strict_eval_source`] of a script's text.
pub fn compile_strict_eval(text: &str) -> Result<Module, String> {
    compile_strict_eval_source(&Source::from_text(text))
}

/// [`compile_direct_eval_source`] of a script's text.
pub fn compile_direct_eval(
    text: &str,
    strict: bool,
    new_target: bool,
    home: bool,
    live_this: bool,
    chain: &str,
    global: bool,
    bound: &[(ast::Name, u8, ast::Name, usize)],
) -> Result<Module, String> {
    compile_direct_eval_source(&Source::from_text(text), strict, new_target, home, live_this, chain, global, bound)
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
        parse::parse_source_code(&Source::of_unit_string(&text), &arena, skim).and_then(|mut p| {
            p.own_names = f.own_names.iter().map(|n| (**n).into()).collect();
            codegen::generate_lazy_units(&p, 2, &f.source, &at, &f.upvalues)
        })
    };
    match compile(true) {
        Err(e) if e.msg == codegen::SKIMMED_TOO_SOON => compile(false),
        r => r,
    }
    .map_err(|e| Error::new(at(e.pos as ast::Pos) as usize, e.msg).display_source(&Source::of_unit_string(&f.source)))
}

/// `String(v)` for a number, as a property key.
pub(crate) fn number_to_string(v: f64) -> String {
    crate::vm::number_to_string(v)
}
