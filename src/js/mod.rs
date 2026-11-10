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

use crate::asm::{Checked, LazyFn, Module};

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
        // (Characters of the text: not the `ESCAPE`s source text puts
        // before some, see `lex::text_to_source`.)
        let (mut col, mut after) = (1, false);
        for c in before.rsplit('\n').next().unwrap().chars() {
            after = !after && c as u32 == lex::ESCAPE;
            col += !after as usize;
        }
        format!("{line}:{col}: {}", self.msg)
    }
}

// The front end takes source text as `lex::text_to_source` has it: a
// string's (`eval`'s, `Function`'s) as `lex::units_to_string` has its code
// units, its lone surrogates code units of their own. `parse`, `compile`,
// `module_info` and `compile_module` take a file's text, and make it that.

/// Parse `src`, a script's text: its syntax error, if any (at an offset of
/// `src`).
pub fn parse(src: &str) -> Result<(), Error> {
    let source = lex::text_to_source(src);
    check_source(&source).map_err(|mut e| {
        // (Back to an offset of `src`: less the `ESCAPE`s put before it,
        // each before a character.)
        if matches!(source, std::borrow::Cow::Owned(_)) {
            let (mut escapes, mut after) = (0, false);
            for c in source[..e.pos.min(source.len())].chars() {
                after = !after && c as u32 == lex::ESCAPE;
                escapes += after as usize;
            }
            e.pos -= escapes * char::from_u32(lex::ESCAPE).unwrap().len_utf8();
        }
        e
    })
}

/// [`parse`] of source text (see `lex::text_to_source`).
pub fn check_source(src: &str) -> Result<(), Error> {
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
pub fn compile(src: &str) -> Result<Checked, String> {
    compile_source(&lex::text_to_source(src))
}

/// [`compile`] of source text (see `lex::text_to_source`).
pub fn compile_source(src: &str) -> Result<Checked, String> {
    let arena = ast::Arenas::default();
    parse::parse_source(src, &arena, false)
        .and_then(|p| codegen::generate(&p, src))
        .map_err(|e| e.display(src))
        // SAFETY: the front end's code is right by construction.
        .map(|m| unsafe { Checked::new_unchecked(m) })
}

/// Module `src`'s requests, imports and exports (module `id`'s); errors
/// are `line:column: message`.
pub fn module_info(src: &str, id: i32) -> Result<ast::ModuleInfo, String> {
    let src = &*lex::text_to_source(src);
    let arena = ast::Arenas::default();
    let p = parse::parse_module(src, &arena, id).map_err(|e| e.display(src))?;
    Ok(p.module.unwrap_or_default())
}

/// Compile module `src` (module `id`, see `module`): a script whose value
/// is the module's function (see `ast::MODULE_FUNCTION`). `links`: per
/// import (see `ast::ModuleInfo::imports`), the variable of the scope
/// object its holder is, or none (the holder is the value).
pub fn compile_module(src: &str, id: i32, links: &[Option<String>]) -> Result<Module, String> {
    let src = &*lex::text_to_source(src);
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

/// Compile `src` as indirect `eval`'s code: as a script's, but its
/// block-scoped bindings its own, and strict code's `var`s and functions.
pub fn compile_indirect_eval(src: &str) -> Result<Module, String> {
    let source: Rc<str> = src.into();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source(src, &arena, skim).and_then(|mut p| {
            p.eval_locals = p.functions[0].strict;
            p.script = false;
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

/// Compile `src` as direct `eval`'s code (`strict`: the caller is;
/// `new_target`: it has a `new.target`, see `parse::parse_source_as`;
/// `home`: a home object, a method's, see `ast::EVAL_HOME`; `live_this`:
/// its `this` the caller's binding, see `ast::EVAL_LIVE_THIS`), in the
/// scopes `chain` (see `parse::parse_pass`): its names looked up in their
/// objects (see `intrinsics::direct_eval`). Or, with `bound`
/// (`ast::Program::eval_upvalues`), also in the caller's bindings, its
/// upvalues (see `intrinsics::direct_eval_bound`).
pub fn compile_direct_eval(
    src: &str,
    strict: bool,
    new_target: bool,
    home: bool,
    live_this: bool,
    chain: &str,
    global: bool,
    bound: &[(ast::Name, u8, ast::Name, usize)],
) -> Result<Module, String> {
    let source: Rc<str> = src.into();
    let compile = |skim| {
        let arena = ast::Arenas::default();
        parse::parse_source_as(src, &arena, skim, Some((strict, chain, !bound.is_empty(), global, new_target, home, live_this))).and_then(|mut p| {
            p.eval_upvalues = bound.to_vec();
            codegen::generate_lazy(&p, 0, &source, &|pos| pos, &[])
        })
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
