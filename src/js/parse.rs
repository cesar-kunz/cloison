//! The parser: tokens into a [`Program`], by recursive descent, with
//! precedence climbing for binary operators.
//!
//! It covers ES5's statements and expressions minus `with`,
//! getters and setters and regular expressions.

use super::Error;
use super::ast::*;
use super::lex::{Tok, Token};

/// `toks`' program, its nodes in `arena` (the text of string and regular
/// expression tokens moves to the tree).
pub fn parse<'a>(toks: &mut [Token], arena: &'a Arenas<'a>) -> Result<Program<'a>, Error> {
    parse_with(toks, arena, false)
}

/// [`parse`], but with `skim`, the body of each function expression or
/// declaration not likely called at once is only skimmed (see
/// [`Function::free`]): it compiles lazily, parsed again then.
pub fn parse_with<'a>(toks: &mut [Token], arena: &'a Arenas<'a>, skim: bool) -> Result<Program<'a>, Error> {
    // A first pass finds each scope's block-scoped declarations; if there
    // are any, a second renames them apart (see `Parser::reference`).
    let lexical = has_lexical(toks);
    let (first, table) = parse_pass(toks, arena, skim && !lexical, true, None, None, None)?;
    if table.trivial() {
        return Ok(first);
    }
    parse_pass(toks, arena, skim, false, Some(table), Some(&first), None).map(|(p, _)| p)
}

/// [`parse_with`] of source `src`, lexed again for a second pass (the
/// first takes the strings out of its tokens).
pub fn parse_source<'a>(src: &str, arena: &'a Arenas<'a>, skim: bool) -> Result<Program<'a>, Error> {
    parse_source_as(src, arena, skim, None)
}

/// [`parse_source`]; `eval`: as direct `eval`'s code (strict or not).
pub fn parse_source_as<'a>(src: &str, arena: &'a Arenas<'a>, skim: bool, eval: Option<bool>) -> Result<Program<'a>, Error> {
    let mut toks = super::lex::lex(src)?;
    let lexical = has_lexical(&toks);
    let (first, table) = parse_pass(&mut toks, arena, skim && !lexical, false, None, None, eval)?;
    if table.trivial() {
        return Ok(first);
    }
    let mut toks = super::lex::lex(src)?;
    parse_pass(&mut toks, arena, skim, false, Some(table), Some(&first), eval).map(|(p, _)| p)
}

/// Whether `toks` has `let`, `const` or `class` (then a first pass parses
/// every function in full, for their `var`s: see [`Parser::push_vars`]),
/// as the lexer marks its last token.
fn has_lexical(toks: &[Token]) -> bool {
    toks.last().is_some_and(|t| t.escaped)
}

/// One pass of [`parse_with`]: the first collects its [`Table`], the
/// second (`table` given) renames by it.
fn parse_pass<'a, 'p>(
    toks: &mut [Token],
    arena: &'a Arenas<'a>,
    skim: bool,
    keep: bool,
    table: Option<Table>,
    prev: Option<&'p Program<'a>>,
    eval: Option<bool>,
) -> Result<(Program<'a>, Table), Error> {
    let first = table.is_none();
    let mut p = Parser {
        toks,
        arena,
        i: 0,
        functions: Vec::new(),
        current: 0,
        skim,
        skim_spare: Vec::new(),
        withs: Vec::new(),
        with_count: 0,
        classes: 0,
        pending_generator: false,
        pending_async: false,
        pending_head: Vec::new(),
        sub: false,
        pending_pattern: None,
        home_uses: 0,
        pending_home: None,
        keep,
        prev,
        table: table.unwrap_or_default(),
        collect: first,
        frames: Vec::new(),
        renames: 0,
        lexicals: Default::default(),
    };
    p.functions.push(Function {
        name: None,
        params: Vec::new(),
        body: Vec::new(),
        parent: None,
        declaration: false,
        binds_name: false,
        uses_arguments: false,
        pos: 0,
        params_pos: 0,
        end: 0,
        eager: false,
        strict: false,
        withs: Vec::new(),
        arrow: false,
        uses_this: false,
        method: false,
        length: 0,
        rest: false,
        param_stmts: 0,
        binding: None,
        annex_b: false,
        class_kind: 0,
        has_eval: false,
        generator: false,
        is_async: false,
        uses_new_target: false,
        hidden: Vec::new(),
        text: None,
        home: None,
        ctor_var: None,
        free: None,
    });
    p.functions[0].strict = p.directive_prologue() || eval == Some(true);
    if eval.is_some() {
        // The caller's scope object, then the eval's own (for strict
        // code's `var`s): what `with`s would give.
        for e in ["%e0", "%e1"] {
            let e: Name = e.into();
            p.hidden_var(e.clone());
            p.lexicals.get_mut(&e).unwrap().withs = 2;
            p.withs.push(e.clone());
            p.functions[0].withs.push(e);
        }
    }
    p.push_scope(usize::MAX, false, true);
    let mut body = Vec::new();
    while p.peek() != &Tok::Eof {
        body.push(p.statement()?);
    }
    p.frames.pop();
    p.functions[0].body = body;
    Ok((
        Program {
            functions: p.functions,
            lexicals: p.lexicals,
            direct_eval: eval.is_some(),
            eval_locals: false,
        },
        p.table,
    ))
}

/// What a scope declares (`let`, `const`, a block's function, a catch
/// parameter: their [`LET`]… kinds), by the token starting it.
#[derive(Default)]
struct Table {
    decls: crate::jit::HashMap<usize, Vec<(Name, u8)>>,
    /// A function has parameter defaults (see `Parser::push_body_vars`).
    defaults: bool,
}

impl Table {
    /// Nothing to rename but catch parameters (renamed only along with
    /// other names).
    fn trivial(&self) -> bool {
        !self.defaults && self.decls.values().flatten().all(|&(_, kind)| kind == CATCH)
    }
}

const LET: u8 = 0;
const CONST: u8 = 1;
const BLOCK_FUNCTION: u8 = 2;
const CATCH: u8 = 4;

/// A scope being parsed: its names, renamed (the same for a function's
/// `var`s, which shadow outer bindings).
struct Frame {
    key: usize,
    names: Vec<(Name, Name)>,
    switch: bool,
    /// A function's body or the script: functions declared directly in
    /// it are `var`-scoped.
    body: bool,
    /// A function's `var`s (and parameters).
    vars: bool,
    /// A catch clause's parameter.
    catch: bool,
}

struct Parser<'t, 'a, 'p> {
    toks: &'t mut [Token],
    /// The first pass's program, in the second (see [`Parser::push_vars`]).
    prev: Option<&'p Program<'a>>,
    arena: &'a Arenas<'a>,
    i: usize,
    functions: Vec<Function<'a>>,
    /// The function whose body is being parsed.
    current: usize,
    /// Skim functions' bodies (see [`parse_with`]).
    skim: bool,
    /// Empty frames for [`Parser::skim_body`] to reuse.
    skim_spare: Vec<SkimFrame>,
    /// The `with` statements being parsed: their variables (see
    /// `StmtKind::With`), outermost first.
    withs: Vec<Name>,
    with_count: usize,
    /// Classes so far (for their home objects' variables' names).
    classes: usize,
    /// The method about to be parsed is a generator (`*m() {}`).
    pending_generator: bool,
    /// The function about to be parsed is async.
    pending_async: bool,
    /// A `for` head pattern's names (see `head_tdz`).
    pending_head: Vec<Name>,
    /// The statement about to be parsed is another's body.
    sub: bool,
    /// A `for` head's declaration's pattern (see `pattern_declarator`).
    pending_pattern: Option<Pattern<'a>>,
    /// `super` property references so far.
    home_uses: usize,
    /// The home object variables for the method about to be parsed (see
    /// `Function::home`).
    pending_home: Option<(Option<Name>, Option<Name>, u8)>,
    /// Leave string tokens' text (a first pass: another follows).
    keep: bool,
    /// Scopes' declarations: being collected (the first pass), or the
    /// first pass's.
    table: Table,
    collect: bool,
    /// The scopes enclosing the code being parsed, innermost last.
    frames: Vec<Frame>,
    renames: usize,
    lexicals: std::collections::HashMap<Name, Lexical>,
}

/// A function being skimmed (see [`Parser::skim_body`]): the names it
/// certainly declares, those it and the functions in it refer to, and
/// the bracket level inside its body.
#[derive(Default)]
struct SkimFrame {
    decls: Vec<Name>,
    refs: Vec<Name>,
    level: usize,
}

type R<T> = Result<T, Error>;

/// `e` in `arena`.
fn expr<'a>(arena: &'a Arenas<'a>, e: Expr<'a>) -> &'a Expr<'a> {
    arena.exprs.alloc(e)
}

/// `s` in `arena`.
fn stmt<'a>(arena: &'a Arenas<'a>, s: Stmt<'a>) -> &'a Stmt<'a> {
    arena.stmts.alloc(s)
}

impl<'a> Parser<'_, 'a, '_> {
    fn peek(&self) -> &Tok {
        &self.toks[self.i].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.i + n).min(self.toks.len() - 1)].tok
    }

    fn pos(&self) -> Pos {
        self.toks[self.i].pos
    }

    /// The current token, and on to the next. A string's or a regular
    /// expression's text moves out: no token is read again, but for its
    /// kind in an error message.
    fn bump(&mut self) -> Tok {
        let t = match &mut self.toks[self.i].tok {
            Tok::Str(s) if !self.keep => Tok::Str(std::mem::take(s)),
            Tok::RegExp(r) if !self.keep => Tok::RegExp(std::mem::take(r)),
            t => t.clone(),
        };
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
        t
    }

    /// The next token, without the current one's copy that `bump` returns.
    fn advance(&mut self) {
        if self.i + 1 < self.toks.len() {
            self.i += 1;
        }
    }

    fn is(&self, p: &str) -> bool {
        matches!(self.peek(), Tok::Punct(q) | Tok::Keyword(q) if *q == p)
    }

    fn eat(&mut self, p: &str) -> bool {
        let yes = self.is(p);
        if yes {
            self.advance();
        }
        yes
    }

    fn err<T>(&self, msg: impl Into<String>) -> R<T> {
        Err(Error::new(self.pos() as usize, msg.into()))
    }

    fn unexpected<T>(&self) -> R<T> {
        let what = match self.peek() {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Keyword(k) | Tok::Punct(k) => format!("`{k}`"),
            Tok::Num(_) => "a number".into(),
            Tok::Str(_) => "a string".into(),
            Tok::RegExp(..) => "a regular expression".into(),
            Tok::Template(_) => "a template".into(),
            Tok::Eof => "end of input".into(),
        };
        self.err(format!("unexpected {what}"))
    }

    fn expect(&mut self, p: &str) -> R<()> {
        if self.eat(p) {
            Ok(())
        } else {
            match self.peek() {
                Tok::Eof => self.err(format!("expected `{p}` before end of input")),
                _ => self.err(format!("expected `{p}`")),
            }
        }
    }

    fn ident(&mut self) -> R<Name> {
        match self.peek() {
            Tok::Ident(_) => match self.bump() {
                Tok::Ident(s) => {
                    self.strict_word(&s)?;
                    Ok(s)
                }
                _ => unreachable!(),
            },
            _ => self.err("expected an identifier"),
        }
    }

    /// An error for a word strict code reserves (just read), if it is one.
    fn strict_word(&self, s: &str) -> R<()> {
        if self.strict()
            && matches!(s, "implements" | "interface" | "package" | "private" | "protected" | "public" | "static")
        {
            return Err(Error::new(self.toks[self.i - 1].pos as usize, format!("`{s}` is reserved in strict mode")));
        }
        Ok(())
    }

    /// A property name after `.`, where reserved words are allowed.
    fn property_name(&mut self) -> R<Name> {
        let start = self.i;
        match self.bump() {
            Tok::Ident(s) => Ok(s),
            Tok::Keyword(k) => Ok(k.into()),
            _ => {
                self.i = start;
                self.err("expected a property name")
            }
        }
    }

    /// A statement's terminating semicolon, or where one is inserted
    /// automatically: before `}`, at the end, or after a line break.
    /// Whether a directive prologue with `"use strict"` starts here: string
    /// expression statements, one of them `"use strict"` (not consumed).
    fn directive_prologue(&mut self) -> bool {
        let mut j = self.i;
        while let Tok::Str(s) = &self.toks[j].tok {
            let next = &self.toks[j + 1];
            let ends = matches!(next.tok, Tok::Punct(";" | "}") | Tok::Eof) || next.newline_before;
            if !ends {
                return false;
            }
            if s == "use strict" && !self.toks[j].escaped {
                return true;
            }
            j += 1;
            if self.toks[j].tok == Tok::Punct(";") {
                j += 1;
            }
        }
        false
    }

    /// Whether the code being parsed is strict.
    fn strict(&self) -> bool {
        self.functions[self.current].strict
    }

    fn semicolon(&mut self) -> R<()> {
        if self.eat(";") || self.is("}") || *self.peek() == Tok::Eof {
            return Ok(());
        }
        if self.toks[self.i].newline_before {
            return Ok(());
        }
        self.unexpected()
    }

    // ---- Statements ----

    fn statement(&mut self) -> R<Stmt<'a>> {
        let pos = self.pos();
        let sub = std::mem::replace(&mut self.sub, false);
        let kind = match self.peek().clone() {
            Tok::Punct("{") => {
                self.advance();
                StmtKind::Block(self.scoped_block_rest()?)
            }
            Tok::Punct(";") => {
                self.advance();
                StmtKind::Empty
            }
            Tok::Keyword("var") => {
                self.advance();
                let decls = self.var_decls(false)?;
                self.semicolon()?;
                StmtKind::Var(decls)
            }
            Tok::Keyword("const") => {
                self.advance();
                let decls = self.lexical_decls(true, false)?;
                self.semicolon()?;
                StmtKind::Lexical(true, decls)
            }
            Tok::Ident(n) if self.let_ahead(&n) && !(sub && self.toks[self.i + 1].newline_before) => {
                self.advance();
                let decls = self.lexical_decls(false, false)?;
                self.semicolon()?;
                StmtKind::Lexical(false, decls)
            }
            Tok::Keyword("class") => {
                self.advance();
                let name = self.ident()?;
                let c = self.class(Some(name.clone()), pos)?;
                let end = self.pos();
                let binding = self.declare(&name, LET, end);
                let e = Expr {
                    kind: ExprKind::Class(Box::new(c)),
                    pos,
                };
                StmtKind::Lexical(false, vec![(binding, Some(e))])
            }
            // (No debugger to stop in.)
            Tok::Keyword("debugger") => {
                self.advance();
                self.semicolon()?;
                StmtKind::Empty
            }
            Tok::Keyword(k @ ("import" | "export")) => {
                return self.err(format!("`{k}` is not supported"));
            }
            Tok::Keyword("with") => {
                if self.strict() {
                    return self.err("`with` in strict mode");
                }
                self.advance();
                self.expect("(")?;
                let o = self.expression(false)?;
                self.expect(")")?;
                let name: Name = format!("%with{}", self.with_count).into();
                self.with_count += 1;
                self.withs.push(name.clone());
                let body = self.sub_statement();
                self.withs.pop();
                StmtKind::With(o, name, stmt(self.arena, body?))
            }
            Tok::Keyword("function") => {
                self.advance();
                let f = self.function(true, false, pos)?;
                if !self.frames.last().unwrap().body {
                    self.block_function(f);
                } else {
                    self.body_function(f);
                }
                StmtKind::Function(f)
            }
            Tok::Ident(n) if self.async_function_ahead(&n) => {
                self.advance();
                self.advance();
                self.pending_async = true;
                let f = self.function(true, false, pos)?;
                if !self.frames.last().unwrap().body {
                    self.block_function(f);
                }
                StmtKind::Function(f)
            }
            Tok::Keyword("if") => {
                self.advance();
                self.expect("(")?;
                let c = self.expression(false)?;
                self.expect(")")?;
                let then = self.if_branch()?;
                let other = if self.eat("else") {
                    Some(self.if_branch()?)
                } else {
                    None
                };
                StmtKind::If(c, then, other)
            }
            Tok::Keyword("for") => {
                self.advance();
                let key = self.i;
                self.push_scope(key, false, false);
                let r = self.for_rest();
                self.frames.pop();
                r?
            }
            Tok::Keyword("while") => {
                self.advance();
                self.expect("(")?;
                let c = self.expression(false)?;
                self.expect(")")?;
                StmtKind::While(c, stmt(self.arena, self.sub_statement()?))
            }
            Tok::Keyword("do") => {
                self.advance();
                let body = stmt(self.arena, self.sub_statement()?);
                self.expect("while")?;
                self.expect("(")?;
                let c = self.expression(false)?;
                self.expect(")")?;
                // `do … while (c)` takes an automatic semicolon even
                // without a line break.
                self.eat(";");
                StmtKind::DoWhile(body, c)
            }
            Tok::Keyword("return") => {
                self.advance();
                if self.current == 0 {
                    return Err(Error::new(
                        pos as usize,
                        "`return` outside a function".into(),
                    ));
                }
                let e = if self.is(";")
                    || self.is("}")
                    || *self.peek() == Tok::Eof
                    || self.toks[self.i].newline_before
                {
                    None
                } else {
                    Some(self.expression(false)?)
                };
                self.semicolon()?;
                StmtKind::Return(e)
            }
            Tok::Keyword(k @ ("break" | "continue")) => {
                self.advance();
                let label = match self.peek() {
                    Tok::Ident(_) if !self.toks[self.i].newline_before => Some(self.ident()?),
                    _ => None,
                };
                self.semicolon()?;
                if k == "break" {
                    StmtKind::Break(label)
                } else {
                    StmtKind::Continue(label)
                }
            }
            Tok::Keyword("try") => {
                self.advance();
                self.expect("{")?;
                let block = self.scoped_block_rest()?;
                let handler = if self.eat("catch") {
                    let key = self.i;
                    self.expect("(")?;
                    self.push_scope(key, false, false);
                    self.frames.last_mut().unwrap().catch = true;
                    let r = (|| {
                        if self.is("[") || self.is("{") {
                            // `catch ([a, b])`: the exception in a variable
                            // of its own, destructured as the block starts.
                            let pos = self.pos();
                            // (A pattern's names do block Annex B's `var`s.)
                            self.frames.last_mut().unwrap().catch = false;
                            let mut names = Vec::new();
                            let mut pat = self.binding_pattern(&mut names)?;
                            let map: Vec<(Name, Name)> = names.iter().map(|n| (n.clone(), self.declare(n, LET, 0))).collect();
                            rename_pattern(&mut pat, &map);
                            let k = self.classes;
                            self.classes += 1;
                            let e: Name = format!("%catch{k}").into();
                            self.hidden_var(e.clone());
                            self.expect(")")?;
                            self.expect("{")?;
                            let mut body = self.scoped_block_rest()?;
                            let mut decls: Vec<(Name, Option<Expr<'a>>)> =
                                map.into_iter().map(|(_, r)| (r, Some(Expr { kind: ExprKind::Bound, pos }))).collect();
                            let value = expr(self.arena, Expr { kind: ExprKind::Ident(e.clone()), pos });
                            decls.push(("".into(), Some(Expr { kind: ExprKind::Destructure(Box::new(pat), value, 2), pos })));
                            body.insert(0, Stmt { kind: StmtKind::Lexical(false, decls), pos });
                            return Ok((e, body));
                        }
                        let name = self.ident()?;
                        let name = self.declare(&name, CATCH, 0);
                        self.expect(")")?;
                        self.expect("{")?;
                        Ok((name, self.scoped_block_rest()?))
                    })();
                    self.frames.pop();
                    Some(r?)
                } else {
                    None
                };
                let finalizer = if self.eat("finally") {
                    self.expect("{")?;
                    Some(self.scoped_block_rest()?)
                } else {
                    None
                };
                if handler.is_none() && finalizer.is_none() {
                    return self.err("`try` without `catch` or `finally`");
                }
                StmtKind::Try(block, handler, finalizer)
            }
            Tok::Keyword("throw") => {
                self.advance();
                if self.toks[self.i].newline_before {
                    return self.err("line break after `throw`");
                }
                let e = self.expression(false)?;
                self.semicolon()?;
                StmtKind::Throw(e)
            }
            Tok::Keyword("switch") => {
                self.advance();
                self.switch_rest()?
            }
            Tok::Ident(name) if self.peek_at(1) == &Tok::Punct(":") => {
                self.advance();
                self.advance();
                StmtKind::Labeled(name, stmt(self.arena, self.sub_statement()?))
            }
            _ => {
                let e = self.expression(false)?;
                self.semicolon()?;
                StmtKind::Expr(e)
            }
        };
        Ok(Stmt { kind, pos })
    }

    /// A statement that is another's body (not a declaration's place:
    /// `let` there is an identifier).
    fn sub_statement(&mut self) -> R<Stmt<'a>> {
        self.sub = true;
        let r = self.statement();
        self.sub = false;
        r
    }

    /// An `if`'s branch: a function declaration (Annex B) as if in a
    /// block of its own.
    fn if_branch(&mut self) -> R<&'a Stmt<'a>> {
        if !self.is("function") {
            return Ok(stmt(self.arena, self.sub_statement()?));
        }
        let pos = self.pos();
        self.push_scope(self.i, false, false);
        let r = self.statement();
        self.frames.pop();
        let block = Stmt {
            kind: StmtKind::Block(vec![r?]),
            pos,
        };
        Ok(stmt(self.arena, block))
    }

    /// Statements up to and including the closing `}`.
    fn block_rest(&mut self) -> R<Vec<Stmt<'a>>> {
        let mut body = Vec::new();
        while !self.eat("}") {
            if *self.peek() == Tok::Eof {
                return self.err("expected `}` before end of input");
            }
            body.push(self.statement()?);
        }
        Ok(body)
    }

    fn var_decls(&mut self, no_in: bool) -> R<Vec<(Name, Option<Expr<'a>>)>> {
        let mut decls = Vec::new();
        loop {
            if self.is("[") || self.is("{") {
                self.pattern_declarator(None, no_in, &mut decls)?;
                if !self.eat(",") {
                    return Ok(decls);
                }
                continue;
            }
            let written = self.ident()?;
            // (A body's `var` renamed apart: see `push_body_vars`.)
            let name = self.resolve(&written).unwrap_or_else(|| written.clone());
            let init = if self.eat("=") {
                let e = self.assignment(no_in)?;
                self.infer_name(&e, &written);
                Some(e)
            } else {
                None
            };
            decls.push((name, init));
            if !self.eat(",") {
                return Ok(decls);
            }
        }
    }

    fn for_rest(&mut self) -> R<StmtKind<'a>> {
        self.expect("(")?;
        let init = if self.is(";") {
            None
        } else if self.is("const") || matches!(self.peek().clone(), Tok::Ident(n) if self.let_ahead(&n)) {
            let constant = self.is("const");
            self.advance();
            let decls = self.lexical_decls(constant, true)?;
            if let Some(p) = self.pending_pattern.take() {
                if self.is_of() {
                    return self.for_of_pattern(if constant { 3 } else { 2 }, p);
                }
                if self.eat("in") {
                    let names: Vec<Name> = decls.iter().filter(|(n, _)| !n.is_empty()).map(|(n, _)| n.clone()).collect();
                    return self.for_in_target(ForInTarget::Pattern(p, Some(constant), names));
                }
            }
            if decls.len() == 1 && decls[0].1.is_none() && self.is_of() {
                let (name, _) = decls.into_iter().next().unwrap();
                return self.for_of_rest(if constant { 3 } else { 2 }, ExprKind::Ident(name));
            }
            if decls.len() == 1 && decls[0].1.is_none() && self.eat("in") {
                let name = decls.into_iter().next().unwrap().0;
                let object = self.expression(false)?;
                self.head_tdz(std::slice::from_ref(&name));
                self.expect(")")?;
                let body = stmt(self.arena, self.sub_statement()?);
                return Ok(StmtKind::ForIn {
                    var: false,
                    fresh: true,
                    name,
                    object,
                    body,
                });
            }
            if constant && decls.iter().any(|d| d.1.is_none()) {
                return self.err("missing initializer in a `const` declaration");
            }
            Some(ForInit::Lexical(constant, decls))
        } else if self.eat("var") {
            let decls = self.var_decls(true)?;
            if let Some(p) = self.pending_pattern.take() {
                if self.is_of() {
                    return self.for_of_pattern(1, p);
                }
                if self.eat("in") {
                    let names: Vec<Name> = decls.iter().filter(|(n, _)| !n.is_empty()).map(|(n, _)| n.clone()).collect();
                    return self.for_in_target(ForInTarget::Pattern(p, None, names));
                }
            }
            if decls.len() == 1 && decls[0].1.is_none() && self.is_of() {
                let (name, _) = decls.into_iter().next().unwrap();
                return self.for_of_rest(1, ExprKind::Ident(name));
            }
            if decls.len() == 1 && decls[0].1.is_none() && self.eat("in") {
                let name = decls.into_iter().next().unwrap().0;
                return self.for_in_rest(true, name);
            }
            Some(ForInit::Var(decls))
        } else {
            let e = self.expression(true)?;
            if (matches!(e.kind, ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..)) || self.call_target(&e))
                && self.is_of()
            {
                return self.for_of_rest(0, e.kind);
            }
            if matches!(e.kind, ExprKind::Array(_) | ExprKind::Object(..)) && self.is_of() {
                let e = expr(self.arena, e);
                let p = self.to_pattern(e)?;
                return self.for_of_pattern(0, p);
            }
            if (matches!(e.kind, ExprKind::Member(..) | ExprKind::Index(..)) || self.call_target(&e)) && self.eat("in") {
                return self.for_in_target(ForInTarget::Expr(e));
            }
            if matches!(e.kind, ExprKind::Array(_) | ExprKind::Object(..)) && self.eat("in") {
                let e = expr(self.arena, e);
                let p = self.to_pattern(e)?;
                return self.for_in_target(ForInTarget::Pattern(p, None, Vec::new()));
            }
            if let ExprKind::Ident(name) = &e.kind
                && self.eat("in")
            {
                let name = name.clone();
                return self.for_in_rest(false, name);
            }
            Some(ForInit::Expr(e))
        };
        self.expect(";")?;
        let test = if self.is(";") {
            None
        } else {
            Some(self.expression(false)?)
        };
        self.expect(";")?;
        let update = if self.is(")") {
            None
        } else {
            Some(self.expression(false)?)
        };
        self.expect(")")?;
        let body = stmt(self.arena, self.sub_statement()?);
        Ok(StmtKind::For {
            init,
            test,
            update,
            body,
        })
    }

    /// Past `in`: `for (target in object) body` with a target other than a
    /// name, as `for (%forinN in object) { target = %forinN; body }`.
    fn for_in_target(&mut self, target: ForInTarget<'a>) -> R<StmtKind<'a>> {
        let pos = self.pos();
        let k = self.classes;
        self.classes += 1;
        let h: Name = format!("%forin{k}").into();
        self.hidden_var(h.clone());
        let object = self.expression(false)?;
        self.expect(")")?;
        let body = self.sub_statement()?;
        let key = expr(self.arena, Expr { kind: ExprKind::Ident(h.clone()), pos });
        let first = match target {
            ForInTarget::Expr(t) => Stmt {
                kind: StmtKind::Expr(Expr { kind: ExprKind::Assign(None, expr(self.arena, t), key), pos }),
                pos,
            },
            ForInTarget::Pattern(p, lexical, names) => {
                let mode = if lexical.is_some() { 2 } else if names.is_empty() { 0 } else { 1 };
                let mut decls: Vec<(Name, Option<Expr<'a>>)> =
                    names.into_iter().map(|n| (n, Some(Expr { kind: ExprKind::Bound, pos }))).collect();
                let d = Expr { kind: ExprKind::Destructure(Box::new(p), key, mode), pos };
                match lexical {
                    Some(constant) => {
                        decls.push(("".into(), Some(d)));
                        Stmt { kind: StmtKind::Lexical(constant, decls), pos }
                    }
                    None if mode == 1 => {
                        decls.push(("".into(), Some(d)));
                        Stmt { kind: StmtKind::Var(decls), pos }
                    }
                    None => Stmt { kind: StmtKind::Expr(d), pos },
                }
            }
        };
        let body = stmt(self.arena, Stmt { kind: StmtKind::Block(vec![first, body]), pos });
        Ok(StmtKind::ForIn {
            var: false,
            fresh: false,
            name: h,
            object,
            body,
        })
    }

    /// A `for` head's block-scoped names (and `pending_head`'s): not
    /// initialized through the head's expression (its end here).
    fn head_tdz(&mut self, names: &[Name]) {
        let end = self.pos();
        let pending = std::mem::take(&mut self.pending_head);
        for n in names.iter().chain(&pending) {
            if let Some(l) = self.lexicals.get_mut(n) {
                l.end = end;
            }
        }
    }

    /// At `of` in a `for` head.
    fn is_of(&self) -> bool {
        matches!(self.peek(), Tok::Ident(n) if &**n == "of") && !self.toks[self.i].escaped
    }

    /// At `of`: `for (target of iterable) body`'s rest.
    fn for_of_rest(&mut self, kind: u8, target: ExprKind<'a>) -> R<StmtKind<'a>> {
        let pos = self.pos();
        self.advance();
        let iterable = self.assignment(false)?;
        match &target {
            ExprKind::Ident(n) => self.head_tdz(std::slice::from_ref(n)),
            _ => self.head_tdz(&[]),
        }
        self.expect(")")?;
        let body = stmt(self.arena, self.sub_statement()?);
        Ok(StmtKind::ForOf {
            kind,
            target: Expr { kind: target, pos },
            pattern: None,
            iterable,
            body,
        })
    }

    /// `for (pattern of iterable) body`'s rest, at `of`.
    fn for_of_pattern(&mut self, kind: u8, p: Pattern<'a>) -> R<StmtKind<'a>> {
        let mut names = Vec::new();
        p.names(&mut names);
        self.pending_head = names;
        let r = self.for_of_rest(kind, ExprKind::Null)?;
        let StmtKind::ForOf { kind, target, iterable, body, .. } = r else { unreachable!() };
        Ok(StmtKind::ForOf {
            kind,
            target,
            pattern: Some(Box::new(p)),
            iterable,
            body,
        })
    }

    /// A declarator with a pattern (`[a, b] = e`): its names (as
    /// `Bound`), then the destructuring (under the name ""). `lexical`:
    /// the names declared so, renamed. In a `for` head (`no_in`) it may
    /// have no initializer: the pattern waits in `pending_pattern`.
    fn pattern_declarator(&mut self, lexical: Option<u8>, no_in: bool, decls: &mut Vec<(Name, Option<Expr<'a>>)>) -> R<()> {
        let pos = self.pos();
        let mut names = Vec::new();
        let mut pat = self.binding_pattern(&mut names)?;
        let init = if self.eat("=") {
            Some(self.assignment(no_in)?)
        } else if no_in {
            None
        } else {
            return self.err("missing initializer in a destructuring declaration");
        };
        let end = self.pos();
        let names: Vec<Name> = match lexical {
            Some(kind) => {
                let map: Vec<(Name, Name)> = names.iter().map(|n| (n.clone(), self.declare(n, kind, end))).collect();
                rename_pattern(&mut pat, &map);
                map.into_iter().map(|(_, r)| r).collect()
            }
            None => names,
        };
        for n in names {
            decls.push((n, Some(Expr { kind: ExprKind::Bound, pos })));
        }
        let mode = if lexical.is_some() { 2 } else { 1 };
        match init {
            Some(e) => {
                let e = expr(self.arena, e);
                decls.push(("".into(), Some(Expr { kind: ExprKind::Destructure(Box::new(pat), e, mode), pos })));
            }
            None => {
                self.pending_pattern = Some(pat);
                decls.push(("".into(), None));
            }
        }
        Ok(())
    }

    /// A binding pattern: a name, `[…]` or `{…}`, its names (as written)
    /// onto `names`.
    fn binding_pattern(&mut self, names: &mut Vec<Name>) -> R<Pattern<'a>> {
        if self.eat("[") {
            let mut elems = Vec::new();
            let mut rest = None;
            while !self.eat("]") {
                if self.eat(",") {
                    elems.push(None);
                    continue;
                }
                if self.eat("...") {
                    rest = Some(Box::new(self.binding_pattern(names)?));
                    self.eat(",");
                    self.expect("]")?;
                    break;
                }
                let p = self.binding_pattern(names)?;
                let d = self.pattern_default()?;
                elems.push(Some((p, d)));
                if !self.is("]") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Array(elems, rest));
        }
        if self.eat("{") {
            let mut props = Vec::new();
            let mut rest = None;
            while !self.eat("}") {
                if self.eat("...") {
                    let n = self.ident()?;
                    names.push(n.clone());
                    rest = Some(Box::new(Pattern::Name(n)));
                    self.eat(",");
                    self.expect("}")?;
                    break;
                }
                let shorthand = matches!(self.peek(), Tok::Ident(_)) && matches!(self.peek_at(1), Tok::Punct("," | "}" | "="));
                let (key, p) = if shorthand {
                    let n = self.ident()?;
                    names.push(n.clone());
                    (PatKey::Name(n.clone()), Pattern::Name(n))
                } else {
                    let key = match self.prop_key()? {
                        PropKey::Name(n) => PatKey::Name(n),
                        PropKey::Computed(e) => PatKey::Computed(expr(self.arena, e)),
                    };
                    self.expect(":")?;
                    (key, self.binding_pattern(names)?)
                };
                let d = self.pattern_default()?;
                props.push((key, p, d));
                if !self.is("}") {
                    self.expect(",")?;
                }
            }
            return Ok(Pattern::Object(props, rest));
        }
        let n = self.ident()?;
        names.push(n.clone());
        Ok(Pattern::Name(n))
    }

    /// A pattern element's `= default`, if any.
    fn pattern_default(&mut self) -> R<Option<&'a Expr<'a>>> {
        if !self.eat("=") {
            return Ok(None);
        }
        let e = self.assignment(false)?;
        Ok(Some(expr(self.arena, e)))
    }

    /// An array or object literal (or a target) as an assignment pattern.
    fn to_pattern(&self, e: &'a Expr<'a>) -> R<Pattern<'a>> {
        let bad = || Err(Error::new(e.pos as usize, "invalid destructuring target".into()));
        Ok(match &e.kind {
            ExprKind::Ident(n) => Pattern::Name(n.clone()),
            ExprKind::Member(..) | ExprKind::Index(..) => Pattern::Target(e),
            ExprKind::Array(es) => {
                let mut elems = Vec::new();
                let mut rest = None;
                for (k, x) in es.iter().enumerate() {
                    match &x.kind {
                        ExprKind::Hole => elems.push(None),
                        ExprKind::Spread(t) if k + 1 == es.len() => rest = Some(Box::new(self.to_pattern(t)?)),
                        ExprKind::Assign(None, t, d) => elems.push(Some((self.to_pattern(t)?, Some(*d)))),
                        _ => elems.push(Some((self.to_pattern(x)?, None))),
                    }
                }
                Pattern::Array(elems, rest)
            }
            ExprKind::Object(ps, _) => {
                let mut props = Vec::new();
                for (k, v) in ps {
                    let key = match k {
                        PropKey::Name(n) => PatKey::Name(n.clone()),
                        PropKey::Computed(e) => PatKey::Computed(e),
                    };
                    match &v.kind {
                        ExprKind::Assign(None, t, d) => props.push((key, self.to_pattern(t)?, Some(*d))),
                        ExprKind::Accessor(..) | ExprKind::Function(_) if !matches!(v.kind, ExprKind::Function(_)) => return bad(),
                        _ => props.push((key, self.to_pattern(v)?, None)),
                    }
                }
                Pattern::Object(props, None)
            }
            _ => return bad(),
        })
    }

    fn for_in_rest(&mut self, var: bool, name: Name) -> R<StmtKind<'a>> {
        let object = self.expression(false)?;
        self.expect(")")?;
        let body = stmt(self.arena, self.sub_statement()?);
        Ok(StmtKind::ForIn {
            var,
            fresh: false,
            name,
            object,
            body,
        })
    }

    fn switch_rest(&mut self) -> R<StmtKind<'a>> {
        self.expect("(")?;
        let d = self.expression(false)?;
        self.expect(")")?;
        let key = self.i;
        self.expect("{")?;
        self.push_scope(key, true, false);
        let r = self.switch_cases();
        self.frames.pop();
        Ok(StmtKind::Switch(d, r?))
    }

    fn switch_cases(&mut self) -> R<Vec<(Option<Expr<'a>>, Vec<Stmt<'a>>)>> {
        let mut cases = Vec::new();
        let mut default = false;
        while !self.eat("}") {
            let test = if self.eat("case") {
                Some(self.expression(false)?)
            } else if self.is("default") {
                if std::mem::replace(&mut default, true) {
                    return self.err("two `default` clauses");
                }
                self.advance();
                None
            } else {
                return self.unexpected();
            };
            self.expect(":")?;
            let mut body = Vec::new();
            while !self.is("case") && !self.is("default") && !self.is("}") {
                if *self.peek() == Tok::Eof {
                    return self.err("expected `}` before end of input");
                }
                body.push(self.statement()?);
            }
            cases.push((test, body));
        }
        Ok(cases)
    }

    /// After `function`: the name (if `declaration`, required), parameters
    /// and body. Returns the function's index. A body is only skimmed when
    /// skimming, unless the function comes right after a `(` (`paren`) or
    /// right before one.
    fn function(&mut self, declaration: bool, paren: bool, pos: Pos) -> R<usize> {
        let generator = self.eat("*") || std::mem::take(&mut self.pending_generator);
        let is_async = std::mem::take(&mut self.pending_async);
        let name = match self.peek() {
            Tok::Ident(_) => Some(self.ident()?),
            _ if declaration => return self.err("expected the function's name"),
            _ => None,
        };
        let index = self.functions.len();
        let binds_name = !declaration && name.is_some();
        let (home, ctor_var, class_kind) = self.pending_home.take().unwrap_or_default();
        self.functions.push(Function {
            name,
            binds_name,
            uses_arguments: false,
            params: Vec::new(),
            body: Vec::new(),
            parent: Some(self.current),
            declaration,
            pos,
            params_pos: self.pos(),
            end: 0,
            eager: false,
            strict: self.functions[self.current].strict,
            withs: self.withs.clone(),
            arrow: false,
            uses_this: false,
            method: false,
            length: 0,
            rest: false,
            param_stmts: 0,
            binding: None,
            annex_b: false,
            class_kind: 0,
            has_eval: false,
            generator: false,
            is_async: false,
            uses_new_target: false,
            hidden: Vec::new(),
        text: None,
            home,
            ctor_var,
            free: None,
        });
        self.functions[index].class_kind = class_kind;
        self.functions[index].uses_new_target = class_kind != 0;
        self.functions[index].generator = generator;
        self.functions[index].is_async = is_async;
        let vars_key = self.i;
        self.expect("(")?;
        self.push_vars(vars_key);
        let r = self.function_rest(index, paren);
        self.frames.pop();
        let (params, body) = r?;
        let f = &mut self.functions[index];
        f.params = params;
        if let Some(body) = body {
            f.body = body;
        }
        Ok(index)
    }

    /// A function's parameters and body, from past its `(`: the body
    /// `None` if skimmed (see `Function::free`).
    #[allow(clippy::type_complexity)]
    fn function_rest(&mut self, index: usize, paren: bool) -> R<(Vec<Name>, Option<Vec<Stmt<'a>>>)> {
        let outer = std::mem::replace(&mut self.current, index);
        let params = self.params();
        self.current = outer;
        let (params, prologue) = params?;
        // (Known while its body is parsed: see `block_function`.)
        self.functions[index].params = params.clone();
        // Defaults' closures don't see the body's `var`s: renamed apart.
        if !prologue.is_empty() && self.collect {
            self.table.defaults = true;
        }
        let body_vars = !prologue.is_empty() && self.push_body_vars(&params);
        self.expect("{")?;
        if self.directive_prologue() {
            self.functions[index].strict = true;
        }
        if self.functions[index].strict {
            // (Its own name too, though written before it was known strict.)
            if let Some(n) = self.functions[index].name.as_deref()
                && matches!(n, "eval" | "arguments")
            {
                return self.err(format!("`{n}` as a function name in strict mode"));
            }
            for (k, p) in params.iter().enumerate() {
                if params[..k].contains(p) {
                    return self.err(format!("duplicate parameter `{p}` in strict mode"));
                }
                if matches!(&**p, "eval" | "arguments") {
                    return self.err(format!("`{p}` as a parameter name in strict mode"));
                }
            }
        }
        // (Not inside a `with`: the function must see its object.)
        if self.skim && !paren && self.withs.is_empty() && prologue.is_empty() && !self.functions[index].rest {
            let start = self.i;
            if let Some(free) = self.skim_body(&params)
                && !self.is("(")
                // (Not if it refers to block-scoped bindings outside it:
                // compiled lazily, it couldn't declare them again.)
                && free.iter().all(|n| self.resolve(n).is_none())
            {
                let end = self.toks[self.i - 1].pos + 1;
                let f = &mut self.functions[index];
                f.end = end;
                f.free = Some(free);
                return Ok((params, None));
            }
            self.i = start;
        }
        let outer = std::mem::replace(&mut self.current, index);
        let body = self.scoped_block_rest();
        self.current = outer;
        if body_vars {
            self.frames.pop();
        }
        let end = self.toks[self.i - 1].pos + 1;
        self.functions[index].end = end;
        let mut body = body?;
        self.functions[index].param_stmts = prologue.len();
        if !prologue.is_empty() {
            body.splice(0..0, prologue);
        }
        Ok((params, Some(body)))
    }

    /// After a function's `(`: its parameters, up to and including the
    /// `)`, and the statements giving those with defaults their values
    /// (`if (p === void 0) p = e;`). Sets the function's `length` and
    /// `rest`; it's the current one.
    fn params(&mut self) -> R<(Vec<Name>, Vec<Stmt<'a>>)> {
        let mut params: Vec<Name> = Vec::new();
        let mut prologue = Vec::new();
        let mut length = None;
        let mut patterns = Vec::new();
        while !self.eat(")") {
            if !params.is_empty() {
                self.expect(",")?;
                if self.eat(")") {
                    break;
                }
            }
            if self.eat("...") {
                let pos = self.pos();
                let p = if self.is("[") || self.is("{") {
                    let p: Name = format!("%param{}", params.len()).into();
                    let mut names = Vec::new();
                    let pat = self.binding_pattern(&mut names)?;
                    patterns.push((p.clone(), pat, names, pos));
                    p
                } else {
                    self.ident()?
                };
                length.get_or_insert(params.len());
                params.push(p);
                self.functions[self.current].rest = true;
                self.expect(")")?;
                break;
            }
            let pos = self.pos();
            let pattern = if self.is("[") || self.is("{") {
                let mut names = Vec::new();
                let pat = self.binding_pattern(&mut names)?;
                Some((pat, names))
            } else {
                None
            };
            let p = match &pattern {
                Some(_) => format!("%param{}", params.len()).into(),
                None => self.ident()?,
            };
            if self.eat("=") {
                length.get_or_insert(params.len());
                let e = self.assignment(false)?;
                self.infer_name(&e, &p);
                prologue.push(self.default_stmt(p.clone(), e, pos));
            }
            if let Some((pat, names)) = pattern {
                patterns.push((p.clone(), pat, names, pos));
            }
            params.push(p);
        }
        // Destructured parameters: `var`s of the function, set in order
        // after the defaults.
        for (p, pat, names, pos) in patterns {
            let mut decls: Vec<(Name, Option<Expr<'a>>)> =
                names.into_iter().map(|n| (n, Some(Expr { kind: ExprKind::Bound, pos }))).collect();
            let value = expr(self.arena, Expr { kind: ExprKind::Ident(p), pos });
            decls.push(("".into(), Some(Expr { kind: ExprKind::Destructure(Box::new(pat), value, 1), pos })));
            prologue.push(Stmt { kind: StmtKind::Var(decls), pos });
        }
        self.functions[self.current].length = length.unwrap_or(params.len());
        Ok((params, prologue))
    }

    /// `if (name === void 0) name = e;`
    fn default_stmt(&mut self, name: Name, e: Expr<'a>, pos: Pos) -> Stmt<'a> {
        let x = |k| Expr { kind: k, pos };
        let zero = expr(self.arena, x(ExprKind::Num(0.0)));
        let undef = expr(self.arena, x(ExprKind::Unary(UnOp::Void, zero)));
        let p1 = expr(self.arena, x(ExprKind::Ident(name.clone())));
        let test = x(ExprKind::Binary(BinOp::StrictEq, p1, undef));
        let p2 = expr(self.arena, x(ExprKind::Ident(name)));
        let assign = x(ExprKind::Assign(None, p2, expr(self.arena, e)));
        let then = stmt(self.arena, Stmt { kind: StmtKind::Expr(assign), pos });
        Stmt { kind: StmtKind::If(test, then, None), pos }
    }

    /// The nearest enclosing function that isn't an arrow (the one whose
    /// `this` and `arguments` an arrow uses).
    fn non_arrow(&self) -> usize {
        let mut k = self.current;
        while self.functions[k].arrow {
            k = self.functions[k].parent.unwrap();
        }
        k
    }

    /// After a `(`: whether `=>` follows its matching `)` (the lexer
    /// marks such a `(`).
    fn arrow_ahead(&self) -> bool {
        self.toks[self.i - 1].arrow
    }

    #[allow(dead_code)]
    fn arrow_ahead_scan(&self) -> bool {
        let (mut j, mut level) = (self.i, 1);
        while j < self.toks.len() {
            match &self.toks[j].tok {
                Tok::Punct("(" | "[" | "{") => level += 1,
                Tok::Punct(")" | "]" | "}") => {
                    level -= 1;
                    if level == 0 {
                        return self.toks.get(j + 1).is_some_and(|t| t.tok == Tok::Punct("=>") && !t.newline_before);
                    }
                }
                Tok::Eof => return false,
                _ => {}
            }
            j += 1;
        }
        false
    }

    /// An arrow function after its parameters, at its `=>`: a block body,
    /// or an expression returned.
    fn arrow(&mut self, single: Option<Name>, pos: Pos) -> R<usize> {
        // (Its parameters' frame: keyed by the token before them.)
        let vars_key = self.i - 1;
        self.push_vars(vars_key);
        let r = self.arrow_rest(single, pos);
        self.frames.pop();
        r
    }

    fn arrow_rest(&mut self, single: Option<Name>, pos: Pos) -> R<usize> {
        let index = self.functions.len();
        self.functions.push(Function {
            name: None,
            binds_name: false,
            uses_arguments: false,
            params: Vec::new(),
            body: Vec::new(),
            parent: Some(self.current),
            declaration: false,
            pos,
            params_pos: pos,
            end: 0,
            eager: false,
            strict: self.functions[self.current].strict,
            withs: self.withs.clone(),
            arrow: true,
            uses_this: false,
            method: false,
            length: 0,
            rest: false,
            param_stmts: 0,
            binding: None,
            annex_b: false,
            class_kind: 0,
            has_eval: false,
            generator: false,
            is_async: std::mem::take(&mut self.pending_async),
            uses_new_target: false,
            hidden: Vec::new(),
        text: None,
            home: None,
            ctor_var: None,
            free: None,
        });
        let outer = std::mem::replace(&mut self.current, index);
        let params = match single {
            Some(n) => {
                self.functions[index].length = 1;
                Ok((vec![n], Vec::new()))
            }
            None => self.params(),
        };
        let (params, prologue) = match params.and_then(|p| self.expect("=>").map(|_| p)) {
            Ok(p) => p,
            Err(e) => {
                self.current = outer;
                return Err(e);
            }
        };
        let body = if self.eat("{") {
            self.scoped_block_rest()
        } else {
            let p = self.pos();
            self.assignment(false).map(|e| {
                vec![Stmt {
                    kind: StmtKind::Return(Some(e)),
                    pos: p,
                }]
            })
        };
        self.current = outer;
        let end = self.toks[self.i - 1].pos + 1;
        let f = &mut self.functions[index];
        f.end = end;
        f.params = params;
        let mut body = body?;
        f.param_stmts = prologue.len();
        if !prologue.is_empty() {
            body.splice(0..0, prologue);
        }
        f.body = body;
        Ok(index)
    }

    /// Past a function's `{`: skip its body, to past its `}`, and return
    /// the names it and the functions in it may refer to from outside it:
    /// every identifier not after a `.`, but `arguments`, less those
    /// certainly declared where it is (parameters; a `var`'s first name,
    /// and those after a comma at its level on the same line). Names it
    /// doesn't use (an object literal's keys, a label, a variable declared
    /// otherwise) only capture variables needlessly. None on what parsing
    /// it in full may reject (the end of the input, `let`).
    fn skim_body(&mut self, params: &[Name]) -> Option<Vec<Name>> {
        // At `(`: a function's parameters, and where its body starts.
        let header = |toks: &[Token], mut j: usize| -> Option<(Vec<Name>, usize)> {
            let mut params = Vec::new();
            if toks[j].tok != Tok::Punct("(") {
                return None;
            }
            j += 1;
            loop {
                match &toks[j].tok {
                    Tok::Punct(")") => break,
                    Tok::Punct(",") => {}
                    Tok::Ident(n) => params.push(n.clone()),
                    _ => return None,
                }
                j += 1;
            }
            (toks[j + 1].tok == Tok::Punct("{")).then_some((params, j + 2))
        };
        // (Frames' vectors kept for the next function's, emptied.)
        let mut spare = std::mem::take(&mut self.skim_spare);
        fn frame(spare: &mut Vec<SkimFrame>, params: &[Name], level: usize) -> SkimFrame {
            let mut f = spare.pop().unwrap_or_default();
            f.decls.extend_from_slice(params);
            f.level = level;
            f
        }
        let mut frames = vec![frame(&mut spare, params, 1)];
        let mut level = 1;
        // The level of the `var` statement being read, while its
        // declarations may follow.
        let mut var_level = None;
        loop {
            let t = &self.toks[self.i];
            if t.newline_before && var_level == Some(level) {
                var_level = None;
            }
            match &t.tok {
                Tok::Eof | Tok::Punct("=>" | "...") | Tok::Keyword("const" | "class" | "with" | "import" | "export" | "super") => return None,
                Tok::Ident(n) if &**n == "let" => return None,
                Tok::Keyword("new") if self.toks[self.i + 1].tok == Tok::Punct(".") => return None,
                Tok::Punct("{" | "(" | "[") => level += 1,
                Tok::Punct(p @ ("}" | ")" | "]")) => {
                    level -= 1;
                    if var_level.is_some_and(|v| level < v) {
                        var_level = None;
                    }
                    if *p == "}" && level + 1 == frames.last().unwrap().level {
                        let mut f = frames.pop().unwrap();
                        let SkimFrame { decls, refs, .. } = &mut f;
                        let free = refs.drain(..).filter(|n| !decls.contains(n));
                        let done = match frames.last_mut() {
                            Some(outer) => {
                                outer.refs.extend(free);
                                None
                            }
                            None => {
                                let mut free: Vec<Name> = free.collect();
                                free.sort_unstable();
                                free.dedup();
                                Some(free)
                            }
                        };
                        f.decls.clear();
                        spare.push(f);
                        if done.is_some() {
                            self.i += 1;
                            self.skim_spare = spare;
                            return done;
                        }
                    }
                }
                Tok::Punct(";") | Tok::Keyword("in") if var_level == Some(level) => var_level = None,
                Tok::Keyword("var") | Tok::Punct(",") if matches!(t.tok, Tok::Keyword("var")) || var_level == Some(level) => {
                    let next = &self.toks[self.i + 1];
                    if let Tok::Ident(n) = &next.tok
                        && (matches!(t.tok, Tok::Keyword("var")) || !next.newline_before)
                    {
                        frames.last_mut().unwrap().decls.push(n.clone());
                        var_level = Some(level);
                        self.i += 2;
                        continue;
                    }
                }
                Tok::Keyword("function") => {
                    let mut j = self.i + 1;
                    if matches!(self.toks[j].tok, Tok::Ident(_)) {
                        j += 1;
                    }
                    let (params, body) = header(self.toks, j)?;
                    level += 1;
                    frames.push(frame(&mut spare, &params, level));
                    self.i = body;
                    continue;
                }
                Tok::Ident(n) => {
                    // An accessor (`get x() { … }`), a function too.
                    let next = &self.toks[self.i + 1].tok;
                    if (&**n == "get" || &**n == "set")
                        && matches!(next, Tok::Ident(_) | Tok::Str(_) | Tok::Num(_) | Tok::Keyword(_))
                        && self.toks[self.i + 2].tok == Tok::Punct("(")
                    {
                        let (params, body) = header(self.toks, self.i + 2)?;
                        level += 1;
                        frames.push(frame(&mut spare, &params, level));
                        self.i = body;
                        continue;
                    }
                    if &**n != "arguments" && self.toks[self.i - 1].tok != Tok::Punct(".") {
                        frames.last_mut().unwrap().refs.push(n.clone());
                    }
                }
                _ => {}
            }
            self.i += 1;
        }
    }

    /// Name an anonymous function expression after what it's assigned to.
    fn infer_name(&mut self, e: &Expr, name: &Name) {
        let i = match &e.kind {
            ExprKind::Function(i) => *i,
            ExprKind::Class(c) => c.constructor,
            _ => return,
        };
        if self.functions[i].name.is_none() {
            self.functions[i].name = Some(name.clone());
        }
    }

    // ---- Expressions ----

    /// `no_in` excludes the `in` operator (in a `for` head).
    fn expression(&mut self, no_in: bool) -> R<Expr<'a>> {
        let mut e = self.assignment(no_in)?;
        while self.is(",") {
            let pos = self.pos();
            self.advance();
            let r = self.assignment(no_in)?;
            e = Expr {
                kind: ExprKind::Comma(expr(self.arena, e), expr(self.arena, r)),
                pos,
            };
        }
        Ok(e)
    }

    fn assignment(&mut self, no_in: bool) -> R<Expr<'a>> {
        if self.functions[self.current].generator
            && matches!(self.peek(), Tok::Ident(n) if &**n == "yield")
            && !self.toks[self.i].escaped
        {
            let pos = self.pos();
            self.advance();
            let delegate = !self.toks[self.i].newline_before && self.eat("*");
            let ends = self.toks[self.i].newline_before && !delegate
                || matches!(self.peek(), Tok::Punct(")" | "]" | "}" | "," | ";" | ":") | Tok::Eof)
                || matches!(self.peek(), Tok::Template(t) if !t.head)
                || self.is("in") && no_in;
            let arg = if ends && !delegate {
                None
            } else {
                let e = self.assignment(no_in)?;
                Some(expr(self.arena, e))
            };
            return Ok(Expr {
                kind: ExprKind::Yield(arg, delegate),
                pos,
            });
        }
        let target = self.conditional(no_in)?;
        let op = match self.peek() {
            Tok::Punct("=") => None,
            Tok::Punct(p) => match compound(p) {
                Some(op) => Some(op),
                None => return Ok(target),
            },
            _ => return Ok(target),
        };
        let pos = self.pos();
        if op.is_none() && matches!(target.kind, ExprKind::Array(_) | ExprKind::Object(..)) {
            self.advance();
            let t = expr(self.arena, target);
            let p = self.to_pattern(t)?;
            let value = self.assignment(no_in)?;
            return Ok(Expr {
                kind: ExprKind::Destructure(Box::new(p), expr(self.arena, value), 0),
                pos,
            });
        }
        // (A call too in sloppy code, Annex B: a `ReferenceError` when run.)
        if !matches!(
            target.kind,
            ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..) | ExprKind::SuperMember(..)
        ) && !self.call_target(&target)
        {
            return self.err("invalid assignment target");
        }
        if let ExprKind::Ident(n) = &target.kind
            && self.strict()
            && matches!(&**n, "eval" | "arguments")
        {
            return self.err(format!("assignment to `{n}` in strict mode"));
        }
        self.advance();
        let value = self.assignment(no_in)?;
        if op.is_none() {
            match &target.kind {
                ExprKind::Ident(n) | ExprKind::Member(_, n) => {
                    let n = n.clone();
                    self.infer_name(&value, &n)
                }
                _ => {}
            }
        }
        Ok(Expr {
            kind: ExprKind::Assign(op, expr(self.arena, target), expr(self.arena, value)),
            pos,
        })
    }

    fn conditional(&mut self, no_in: bool) -> R<Expr<'a>> {
        let c = self.binary(0, no_in)?;
        if !self.is("?") {
            return Ok(c);
        }
        let pos = self.pos();
        self.advance();
        let a = self.assignment(false)?;
        self.expect(":")?;
        let b = self.assignment(no_in)?;
        Ok(Expr {
            kind: ExprKind::Cond(expr(self.arena, c), expr(self.arena, a), expr(self.arena, b)),
            pos,
        })
    }

    /// Binary operators binding tighter than `min`.
    fn binary(&mut self, min: u8, no_in: bool) -> R<Expr<'a>> {
        let mut l = self.unary()?;
        loop {
            let Some((prec, op)) = binary_op(self.peek()) else {
                return Ok(l);
            };
            if prec <= min || (no_in && matches!(op, Bin::Op(BinOp::In))) {
                return Ok(l);
            }
            let pos = self.pos();
            self.advance();
            // `**` is right-associative.
            let rmin = if matches!(op, Bin::Op(BinOp::Exp)) {
                prec - 1
            } else {
                prec
            };
            let r = self.binary(rmin, no_in)?;
            let kind = match op {
                Bin::Op(op) => ExprKind::Binary(op, expr(self.arena, l), expr(self.arena, r)),
                Bin::And => ExprKind::Logical(true, expr(self.arena, l), expr(self.arena, r)),
                Bin::Or => ExprKind::Logical(false, expr(self.arena, l), expr(self.arena, r)),
            };
            l = Expr { kind, pos };
        }
    }

    fn unary(&mut self) -> R<Expr<'a>> {
        let pos = self.pos();
        if self.functions[self.current].is_async
            && matches!(self.peek(), Tok::Ident(n) if &**n == "await")
            && !self.toks[self.i].escaped
        {
            self.advance();
            let e = self.unary()?;
            return Ok(Expr {
                kind: ExprKind::Await(expr(self.arena, e)),
                pos,
            });
        }
        let op = match self.peek() {
            Tok::Punct("!") => UnOp::Not,
            Tok::Punct("-") => UnOp::Neg,
            Tok::Punct("+") => UnOp::Plus,
            Tok::Punct("~") => UnOp::BitNot,
            Tok::Keyword("typeof") => UnOp::Typeof,
            Tok::Keyword("void") => UnOp::Void,
            Tok::Keyword("delete") => UnOp::Delete,
            Tok::Punct(p @ ("++" | "--")) => {
                let d = if *p == "++" { 1 } else { -1 };
                self.advance();
                let target = self.unary()?;
                self.check_update_target(&target)?;
                return Ok(Expr {
                    kind: ExprKind::Update(expr(self.arena, target), d, true),
                    pos,
                });
            }
            _ => return self.postfix(),
        };
        self.advance();
        let e = self.unary()?;
        if self.is("**") {
            return self.err("parenthesize a unary operand of `**`");
        }
        Ok(Expr {
            kind: ExprKind::Unary(op, expr(self.arena, e)),
            pos,
        })
    }

    /// Whether `e` is a call sloppy code may assign to (Annex B).
    fn call_target(&self, e: &Expr) -> bool {
        matches!(e.kind, ExprKind::Call(..)) && !self.strict()
    }

    fn check_update_target(&self, e: &Expr) -> R<()> {
        match e.kind {
            ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..) | ExprKind::SuperMember(..) => Ok(()),
            _ if self.call_target(e) => Ok(()),
            _ => Err(Error::new(
                e.pos as usize,
                "invalid increment target".into(),
            )),
        }
    }

    fn postfix(&mut self) -> R<Expr<'a>> {
        let e = self.call()?;
        if let Tok::Punct(p @ ("++" | "--")) = self.peek()
            && !self.toks[self.i].newline_before
        {
            let d = if *p == "++" { 1 } else { -1 };
            let pos = self.pos();
            self.check_update_target(&e)?;
            self.advance();
            return Ok(Expr {
                kind: ExprKind::Update(expr(self.arena, e), d, false),
                pos,
            });
        }
        Ok(e)
    }

    /// Member accesses and calls.
    fn call(&mut self) -> R<Expr<'a>> {
        let mut e = if self.is("new") {
            self.new_expr()?
        } else {
            self.primary()?
        };
        loop {
            let pos = self.pos();
            if self.eat("(") {
                let args = self.arguments()?;
                if matches!(&e.kind, ExprKind::Ident(n) if &**n == "eval") {
                    // A direct `eval` (see `Function::has_eval`), which may
                    // read `arguments`.
                    self.functions[self.current].has_eval = true;
                    let k = self.non_arrow();
                    self.functions[k].uses_arguments = true;
                    if self.functions[self.current].arrow {
                        self.functions[k].uses_this = true;
                    }
                }
                e = Expr {
                    kind: ExprKind::Call(expr(self.arena, e), args),
                    pos,
                };
            } else if matches!(self.peek(), Tok::Template(t) if t.head) {
                let Tok::Template(part) = self.bump() else { unreachable!() };
                let (parts, subs) = self.template_rest(part)?;
                let mut args = vec![Expr { kind: ExprKind::TemplateObject(parts), pos }];
                args.extend(subs);
                e = Expr {
                    kind: ExprKind::Call(expr(self.arena, e), args),
                    pos,
                };
            } else if !self.member_suffix(&mut e)? {
                return Ok(e);
            }
        }
    }

    /// `.name` or `[key]` applied to `e`, if one follows.
    fn member_suffix(&mut self, e: &mut Expr<'a>) -> R<bool> {
        let pos = self.pos();
        let kind = if self.eat(".") {
            let name = self.property_name()?;
            ExprKind::Member(expr(self.arena, take(e)), name)
        } else if self.eat("[") {
            let k = self.expression(false)?;
            self.expect("]")?;
            ExprKind::Index(expr(self.arena, take(e)), expr(self.arena, k))
        } else {
            return Ok(false);
        };
        *e = Expr { kind, pos };
        Ok(true)
    }

    /// `new C(args)`, where `C` is a member expression without calls, and
    /// the arguments are optional.
    fn new_expr(&mut self) -> R<Expr<'a>> {
        let pos = self.pos();
        self.expect("new")?;
        if self.is(".") && matches!(self.peek_at(1), Tok::Ident(n) if &**n == "target") {
            self.advance();
            self.advance();
            let k = self.non_arrow();
            if k == 0 {
                return self.err("`new.target` outside a function");
            }
            self.functions[k].uses_new_target = true;
            let mut e = Expr {
                kind: ExprKind::Ident("%newtarget".into()),
                pos,
            };
            while self.member_suffix(&mut e)? {}
            return Ok(e);
        }
        let mut c = if self.is("new") {
            self.new_expr()?
        } else {
            self.primary()?
        };
        // (A tagged template is a member expression too: `new tag`x``.)
        loop {
            if matches!(self.peek(), Tok::Template(t) if t.head) {
                let p = self.pos();
                let Tok::Template(part) = self.bump() else { unreachable!() };
                let (parts, subs) = self.template_rest(part)?;
                let mut args = vec![Expr { kind: ExprKind::TemplateObject(parts), pos: p }];
                args.extend(subs);
                c = Expr { kind: ExprKind::Call(expr(self.arena, c), args), pos: p };
            } else if !self.member_suffix(&mut c)? {
                break;
            }
        }
        let args = if self.eat("(") {
            self.arguments()?
        } else {
            Vec::new()
        };
        Ok(Expr {
            kind: ExprKind::New(expr(self.arena, c), args),
            pos,
        })
    }

    /// After `(`: arguments up to and including `)`.
    fn arguments(&mut self) -> R<Vec<Expr<'a>>> {
        let mut args = Vec::new();
        while !self.eat(")") {
            if !args.is_empty() {
                self.expect(",")?;
                if self.eat(")") {
                    break;
                }
            }
            let p = self.pos();
            if self.eat("...") {
                let x = self.assignment(false)?;
                args.push(Expr { kind: ExprKind::Spread(expr(self.arena, x)), pos: p });
            } else {
                args.push(self.assignment(false)?);
            }
        }
        Ok(args)
    }

    fn primary(&mut self) -> R<Expr<'a>> {
        let pos = self.pos();
        let start = self.i;
        let kind = match self.bump() {
            Tok::Num(v) => ExprKind::Num(v),
            Tok::Str(s) => ExprKind::Str(s),
            Tok::Ident(s) if self.is("=>") && !self.toks[self.i].newline_before => {
                ExprKind::Function(self.arrow(Some(s), pos)?)
            }
            Tok::Ident(s) if &*s == "async" && !self.toks[self.i - 1].escaped && !self.toks[self.i].newline_before && {
                matches!(self.peek(), Tok::Keyword("function"))
                    || matches!(self.peek(), Tok::Ident(_)) && self.peek_at(1) == &Tok::Punct("=>")
                    || self.is("(") && self.toks[self.i].arrow
            } =>
            {
                if self.eat("function") {
                    self.pending_async = true;
                    let f = self.function(false, false, pos)?;
                    ExprKind::Function(f)
                } else if self.eat("(") {
                    self.pending_async = true;
                    ExprKind::Function(self.arrow(None, pos)?)
                } else {
                    let n = self.ident()?;
                    self.pending_async = true;
                    ExprKind::Function(self.arrow(Some(n), pos)?)
                }
            }
            Tok::Ident(s) => {
                self.strict_word(&s)?;
                self.reference(s)
            }
            Tok::Template(part) => {
                let (parts, subs) = self.template_rest(part)?;
                let mut strings = Vec::new();
                for (cooked, _) in parts {
                    match cooked {
                        Some(c) => strings.push(c),
                        None => {
                            return Err(Error::new(pos as usize, "invalid escape in a template literal".into()));
                        }
                    }
                }
                ExprKind::Template(strings, subs)
            }
            Tok::Keyword("this") if self.functions[self.current].arrow => {
                let k = self.non_arrow();
                self.functions[k].uses_this = true;
                ExprKind::Ident("%this".into())
            }
            Tok::Keyword("this") => ExprKind::This,
            Tok::Keyword("class") => {
                let name = match self.peek() {
                    Tok::Ident(_) => Some(self.ident()?),
                    _ => None,
                };
                ExprKind::Class(Box::new(self.class(name, pos)?))
            }
            Tok::Keyword("super") => return self.super_expr(pos),
            Tok::Keyword("true") => ExprKind::Bool(true),
            Tok::Keyword("false") => ExprKind::Bool(false),
            Tok::Keyword("null") => ExprKind::Null,
            Tok::Keyword("function") => {
                // `(function …` or `function () { … }(`: likely called at
                // once (see `Function::eager`).
                let paren = start > 0 && matches!(self.toks[start - 1].tok, Tok::Punct("("));
                let f = self.function(false, paren, pos)?;
                self.functions[f].eager = paren || self.is("(");
                ExprKind::Function(f)
            }
            Tok::Punct("(") if self.arrow_ahead() => ExprKind::Function(self.arrow(None, pos)?),
            Tok::Punct("(") => {
                let e = self.expression(false)?;
                self.expect(")")?;
                return Ok(e);
            }
            Tok::Punct("[") => {
                let mut elems = Vec::new();
                while !self.eat("]") {
                    let p = self.pos();
                    if self.eat(",") {
                        elems.push(Expr { kind: ExprKind::Hole, pos: p });
                        continue;
                    }
                    let e = if self.eat("...") {
                        let x = self.assignment(false)?;
                        Expr { kind: ExprKind::Spread(expr(self.arena, x)), pos: p }
                    } else {
                        self.assignment(false)?
                    };
                    elems.push(e);
                    if !self.is("]") {
                        self.expect(",")?;
                    }
                }
                ExprKind::Array(elems)
            }
            Tok::Punct("{") => {
                let mut props: Vec<(PropKey<'a>, Expr<'a>)> = Vec::new();
                let uses = self.home_uses;
                let mut home: Option<Name> = None;
                while !self.eat("}") {
                    let ppos = self.pos();
                    // `get k() {…}` / `set k(v) {…}`.
                    let accessor = match self.peek() {
                        Tok::Ident(n) if matches!(&**n, "get" | "set") => {
                            let set = &**n == "set";
                            (!matches!(self.peek_at(1), Tok::Punct(":" | "(" | "," | "}" | "="))).then_some(set)
                        }
                        _ => None,
                    };
                    if matches!(self.peek(), Tok::Ident(n) if &**n == "async")
                        && !self.toks[self.i + 1].newline_before
                        && !matches!(self.peek_at(1), Tok::Punct("(" | ":" | "," | "}" | "="))
                    {
                        // `async m() {…}`.
                        self.advance();
                        let generator = self.eat("*");
                        let key = self.prop_key()?;
                        let h = self.object_home(&mut home);
                        self.pending_home = Some((Some(h), None, 0));
                        self.pending_async = true;
                        self.pending_generator = generator;
                        let f = self.method(&key, None, ppos)?;
                        props.push((key, Expr { kind: ExprKind::Function(f), pos: ppos }));
                        if !self.is("}") {
                            self.expect(",")?;
                        }
                        continue;
                    }
                    if self.is("*") {
                        // `*m() {…}`: a generator method.
                        self.advance();
                        let key = self.prop_key()?;
                        let h = self.object_home(&mut home);
                        self.pending_home = Some((Some(h), None, 0));
                        self.pending_generator = true;
                        let f = self.method(&key, None, ppos)?;
                        props.push((key, Expr { kind: ExprKind::Function(f), pos: ppos }));
                        if !self.is("}") {
                            self.expect(",")?;
                        }
                        continue;
                    }
                    if let Some(set) = accessor {
                        self.advance();
                        let key = self.prop_key()?;
                        let prefix = if set { "set" } else { "get" };
                        let h = self.object_home(&mut home);
                        self.pending_home = Some((Some(h), None, 0));
                        let f = self.method(&key, Some(prefix), ppos)?;
                        props.push((key, Expr { kind: ExprKind::Accessor(set, f), pos: ppos }));
                    } else {
                        let shorthand = match self.peek() {
                            Tok::Ident(n) if matches!(self.peek_at(1), Tok::Punct("," | "}" | "=")) => Some(n.clone()),
                            _ => None,
                        };
                        if let Some(n) = shorthand {
                            self.advance();
                            let kind = self.reference(n.clone());
                            let mut v = Expr { kind, pos: ppos };
                            // `{a = 1}`: only as a pattern (see `to_pattern`).
                            if self.eat("=") {
                                let d = self.assignment(false)?;
                                v = Expr {
                                    kind: ExprKind::Assign(None, expr(self.arena, v), expr(self.arena, d)),
                                    pos: ppos,
                                };
                            }
                            props.push((PropKey::Name(n), v));
                        } else {
                            let key = self.prop_key()?;
                            if self.is("(") {
                                let h = self.object_home(&mut home);
                                self.pending_home = Some((Some(h), None, 0));
                                let f = self.method(&key, None, ppos)?;
                                props.push((key, Expr { kind: ExprKind::Function(f), pos: ppos }));
                            } else {
                                self.expect(":")?;
                                let v = self.assignment(false)?;
                                if let PropKey::Name(k) = &key {
                                    self.infer_name(&v, k);
                                }
                                props.push((key, v));
                            }
                        }
                    }
                    if !self.is("}") {
                        self.expect(",")?;
                    }
                }
                // (A home object variable only if `super` used it.)
                let home = home.filter(|_| self.home_uses != uses);
                if let Some(h) = &home {
                    self.hidden_var(h.clone());
                }
                ExprKind::Object(props, home)
            }
            Tok::RegExp(r) => ExprKind::RegExp(r.0, r.1),
            _ => {
                self.i = start;
                return self.unexpected();
            }
        };
        Ok(Expr { kind, pos })
    }
}

impl<'a> Parser<'_, 'a, '_> {
    /// A reference to the variable `name`: to the block-scoped binding it
    /// names, if any, by that binding's new name (each is renamed apart:
    /// `x%3`, a function's variable as any other, which the code
    /// generator knows from [`Program::lexicals`]).
    fn reference(&mut self, name: Name) -> ExprKind<'a> {
        if &*name == "arguments" {
            // (An arrow's are the enclosing function's.)
            let k = self.non_arrow();
            self.functions[k].uses_arguments = true;
        }
        match self.resolve(&name) {
            Some(r) => ExprKind::Ident(r),
            None => ExprKind::Ident(name),
        }
    }

    /// The new name of the block-scoped binding `name` refers to here, if
    /// it refers to one.
    fn resolve(&self, name: &Name) -> Option<Name> {
        if self.collect || self.renames == 0 {
            return None;
        }
        for f in self.frames.iter().rev() {
            if let Some((_, r)) = f.names.iter().find(|(o, _)| o == name) {
                return (r != name).then(|| r.clone());
            }
        }
        None
    }

    /// Enter the scope starting at token `key`.
    fn push_scope(&mut self, key: usize, switch: bool, body: bool) {
        let mut names = Vec::new();
        if !self.collect
            && let Some(decls) = self.table.decls.get(&key)
        {
            for (n, _) in decls {
                if names.iter().any(|(o, _): &(Name, Name)| o == n) {
                    continue;
                }
                let r: Name = format!("{n}%{}", self.renames).into();
                self.renames += 1;
                names.push((n.clone(), r));
            }
        }
        self.frames.push(Frame {
            key,
            names,
            switch,
            body,
            vars: false,
            catch: false,
        });
    }

    /// The `var`s and functions a function with parameter defaults declares
    /// in its body (its first pass's), but its parameters, renamed apart
    /// (its frame pushed): whether there were any.
    fn push_body_vars(&mut self, params: &[Name]) -> bool {
        let Some(prev) = self.prev else { return false };
        let Some(vars) = self.frames.iter().rev().find(|f| f.vars) else { return false };
        let pos = self.toks[vars.key].pos;
        let fs = &prev.functions;
        let k = fs[1..].partition_point(|f| f.params_pos < pos) + 1;
        let Some(f) = fs.get(k).filter(|f| f.params_pos == pos) else { return false };
        let own = f.name.clone().filter(|_| f.binds_name);
        let mut names = Vec::new();
        for n in var_names(fs, k) {
            if params.contains(&n) || &*n == "arguments" || Some(&n) == own.as_ref() || names.iter().any(|(o, _)| *o == n) {
                continue;
            }
            let r: Name = format!("{n}%{}", self.renames).into();
            self.renames += 1;
            names.push((n, r));
        }
        if names.is_empty() {
            return false;
        }
        // (A `var`s frame: the body's scope, inside it, is the function's.)
        self.frames.push(Frame {
            key: usize::MAX - 1,
            names,
            switch: false,
            body: false,
            vars: true,
            catch: false,
        });
        true
    }

    /// Enter a function's parameters and `var`s (keyed by the token
    /// before its parameters), which shadow outer block-scoped bindings.
    /// (The names come from the first pass's function starting there.)
    fn push_vars(&mut self, key: usize) {
        let names = match self.prev {
            Some(prev) => {
                let pos = self.toks[key].pos;
                let fs = &prev.functions;
                let k = fs[1..].partition_point(|f| f.params_pos < pos) + 1;
                match fs.get(k) {
                    Some(f) if f.params_pos == pos => var_names(fs, k).into_iter().map(|n| (n.clone(), n)).collect(),
                    _ => Vec::new(),
                }
            }
            None => Vec::new(),
        };
        self.frames.push(Frame {
            key,
            names,
            switch: false,
            body: false,
            vars: true,
            catch: false,
        });
    }

    /// Past a `{`: a block's statements, in its own scope (a function's
    /// body if in its parameters' frame).
    fn scoped_block_rest(&mut self) -> R<Vec<Stmt<'a>>> {
        let key = self.i - 1;
        let body = self.frames.last().is_some_and(|f| f.vars);
        self.push_scope(key, false, body);
        let r = self.block_rest();
        self.frames.pop();
        r
    }

    /// Declare block-scoped `name` (of kind [`LET`]…) in the innermost
    /// scope, its declaration ending at `end`: its new name.
    fn declare(&mut self, name: &Name, kind: u8, end: Pos) -> Name {
        let frame = self.frames.last().unwrap();
        if self.collect {
            let key = frame.key;
            self.table.decls.entry(key).or_default().push((name.clone(), kind));
            return name.clone();
        }
        let Some((_, r)) = frame.names.iter().find(|(o, _)| o == name) else {
            return name.clone();
        };
        let r = r.clone();
        let switch = frame.switch;
        self.lexicals.insert(
            r.clone(),
            Lexical {
                original: name.clone(),
                constant: kind == CONST,
                hoisted: kind == BLOCK_FUNCTION || kind == CATCH,
                end,
                func: self.current,
                switch,
                withs: self.withs.len(),
            },
        );
        r
    }

    /// Function declaration `f` in a block: its binding in the block, and
    /// in sloppy mode its Annex B `var` (unless a block-scoped binding
    /// between the block and the function has its name).
    fn block_function(&mut self, f: usize) {
        let name = self.functions[f].name.clone().unwrap();
        let binding = self.declare(&name, BLOCK_FUNCTION, 0);
        if self.collect {
            return;
        }
        // (Not a parameter's name.)
        let mut annex_b = !self.functions[self.current].strict && !self.functions[self.current].params.contains(&name);
        for frame in self.frames.iter().rev().skip(1) {
            if frame.vars {
                break;
            }
            if !frame.catch && frame.names.iter().any(|(o, _)| *o == name) {
                annex_b = false;
            }
            if frame.body {
                break;
            }
        }
        if binding != name {
            let func = &mut self.functions[f];
            func.binding = Some(binding);
            func.annex_b = annex_b;
        }
    }

    /// A function declared in a body whose `var`s are renamed apart (see
    /// `push_body_vars`): bound by its new name, made where the body starts.
    fn body_function(&mut self, f: usize) {
        let name = self.functions[f].name.clone().unwrap();
        if let Some(r) = self.resolve(&name) {
            let func = &mut self.functions[f];
            func.binding = Some(r);
            func.annex_b = false;
        }
    }

    /// Whether `async`, the current token, starts a function declaration.
    fn async_function_ahead(&self, n: &Name) -> bool {
        &**n == "async"
            && !self.toks[self.i].escaped
            && self.peek_at(1) == &Tok::Keyword("function")
            && !self.toks[self.i + 1].newline_before
    }

    /// Whether `let`, the current token, starts a declaration.
    fn let_ahead(&self, n: &Name) -> bool {
        &**n == "let"
            && !self.toks[self.i].escaped
            && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[" | "{"))
    }

    /// Past `let` or `const`: the declarations (renamed), with their
    /// initializers. `no_in`: in a `for` head.
    fn lexical_decls(&mut self, constant: bool, no_in: bool) -> R<Vec<(Name, Option<Expr<'a>>)>> {
        let kind = if constant { CONST } else { LET };
        let mut decls = Vec::new();
        loop {
            if self.is("[") || self.is("{") {
                self.pattern_declarator(Some(kind), no_in, &mut decls)?;
                if !self.eat(",") {
                    return Ok(decls);
                }
                continue;
            }
            let name = self.ident()?;
            if &*name == "let" {
                return self.err("`let` can't be declared with `let` or `const`");
            }
            let init = if self.eat("=") {
                let e = self.assignment(no_in)?;
                self.infer_name(&e, &name);
                Some(e)
            } else {
                if constant && !no_in {
                    return self.err("missing initializer in a `const` declaration");
                }
                None
            };
            let end = self.pos();
            let r = self.declare(&name, kind, end);
            decls.push((r, init));
            if !self.eat(",") {
                return Ok(decls);
            }
        }
    }



    /// An object literal's (or a class's) property key: a name, a string,
    /// a number, or `[e]`.
    fn prop_key(&mut self) -> R<PropKey<'a>> {
        let start = self.i;
        Ok(PropKey::Name(match self.bump() {
            Tok::Ident(s) => s,
            Tok::Str(s) => s.into(),
            Tok::Keyword(k) => k.into(),
            Tok::Num(v) => super::number_to_string(v).into(),
            Tok::Punct("[") => {
                let e = self.assignment(false)?;
                self.expect("]")?;
                return Ok(PropKey::Computed(e));
            }
            _ => {
                self.i = start;
                return self.err("expected a property name");
            }
        }))
    }

    /// A method at its `(`: named after its key (`prefix` first: `get`,
    /// `set`).
    fn method(&mut self, key: &PropKey<'a>, prefix: Option<&str>, pos: Pos) -> R<usize> {
        if !self.is("(") {
            return self.err("expected `(`");
        }
        let f = self.function(false, false, pos)?;
        let func = &mut self.functions[f];
        func.method = true;
        func.binds_name = false;
        if let PropKey::Name(k) = key {
            func.name = Some(match prefix {
                Some(p) => format!("{p} {k}").into(),
                None => k.clone(),
            });
        }
        Ok(f)
    }

    /// After `class` and its name (if any): the class. Its code is strict.
    fn class(&mut self, name: Option<Name>, pos: Pos) -> R<Class<'a>> {
        let outer_strict = std::mem::replace(&mut self.functions[self.current].strict, true);
        let key = self.i;
        self.push_scope(key, false, false);
        let r = self.class_rest(name, pos);
        self.frames.pop();
        self.functions[self.current].strict = outer_strict;
        r
    }

    fn class_rest(&mut self, name: Option<Name>, pos: Pos) -> R<Class<'a>> {
        let heritage = if self.eat("extends") {
            Some(self.call()?)
        } else {
            None
        };
        let k = self.classes;
        self.classes += 1;
        let home: Name = format!("%proto{k}").into();
        let home_static: Name = format!("%ctor{k}").into();
        for n in [&home, &home_static] {
            self.hidden_var(n.clone());
        }
        let class_kind = if heritage.is_some() { 3 } else { 2 };
        self.expect("{")?;
        let mut constructor = None;
        let mut members = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            let is_static = matches!(self.peek(), Tok::Ident(n) if &**n == "static")
                && !matches!(self.peek_at(1), Tok::Punct("(" | "="));
            if is_static {
                self.advance();
            }
            let mpos = self.pos();
            let is_async = matches!(self.peek(), Tok::Ident(n) if &**n == "async")
                && !self.toks[self.i + 1].newline_before
                && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}"));
            if is_async {
                self.advance();
            }
            let generator = self.eat("*");
            let kind = match self.peek() {
                Tok::Ident(n) if matches!(&**n, "get" | "set") && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}")) => {
                    let k = if &**n == "get" { 1 } else { 2 };
                    self.advance();
                    k
                }
                _ => 0,
            };
            let key = self.prop_key()?;
            let is_ctor = !is_static && kind == 0 && matches!(&key, PropKey::Name(n) if &**n == "constructor");
            if is_ctor {
                if constructor.is_some() {
                    return self.err("a class with two constructors");
                }
                self.pending_home = Some((Some(home.clone()), Some(home_static.clone()), class_kind));
                let f = self.method(&key, None, mpos)?;
                let func = &mut self.functions[f];
                func.method = false;
                func.class_kind = class_kind;
                func.uses_new_target = true;
                func.name = name.clone();
                constructor = Some(f);
                continue;
            }
            let h = if is_static { &home_static } else { &home };
            self.pending_home = Some((Some(h.clone()), None, 0));
            self.pending_generator = generator;
            self.pending_async = is_async;
            let prefix = match kind {
                1 => Some("get"),
                2 => Some("set"),
                _ => None,
            };
            let f = self.method(&key, prefix, mpos)?;
            members.push(ClassMember {
                key,
                kind,
                is_static,
                function: f,
            });
        }
        let end = self.toks[self.i - 1].pos;
        let constructor = match constructor {
            Some(f) => f,
            None => self.default_constructor(name.clone(), class_kind, &home, &home_static, end),
        };
        // (Its text is the class's, from `class`.)
        self.functions[constructor].text = Some((pos, end + 1));
        let inner = match &name {
            Some(n) => {
                let r = self.declare(n, CONST, end + 1);
                (r != *n).then_some(r)
            }
            None => None,
        };
        Ok(Class {
            inner,
            heritage,
            constructor,
            members,
            home,
            home_static,
        })
    }

    /// A variable of the current function's, for the parser's own use
    /// (see `Function::hidden`).
    fn hidden_var(&mut self, n: Name) {
        self.functions[self.current].hidden.push(n.clone());
        self.lexicals.insert(
            n.clone(),
            Lexical {
                original: n,
                constant: false,
                hoisted: true,
                end: 0,
                func: self.current,
                switch: false,
                withs: 0,
            },
        );
    }

    /// An object literal's home object variable (see `ExprKind::Object`),
    /// named when first needed.
    fn object_home(&mut self, home: &mut Option<Name>) -> Name {
        home.get_or_insert_with(|| {
            let k = self.classes;
            self.classes += 1;
            format!("%obj{k}").into()
        })
        .clone()
    }

    /// A class's implicit constructor: `constructor() {}`, or a derived
    /// class's `constructor(...args) { super(...args); }`.
    fn default_constructor(&mut self, name: Option<Name>, class_kind: u8, home: &Name, home_static: &Name, end: Pos) -> usize {
        let derived = class_kind == 3;
        let mut body = Vec::new();
        if derived {
            let args = expr(self.arena, Expr { kind: ExprKind::Ident("args".into()), pos: end });
            let spread = Expr { kind: ExprKind::Spread(args), pos: end };
            body.push(Stmt {
                kind: StmtKind::Expr(Expr {
                    kind: ExprKind::SuperCall(home_static.clone(), vec![spread]),
                    pos: end,
                }),
                pos: end,
            });
        }
        let index = self.functions.len();
        self.functions.push(Function {
            name,
            params: if derived { vec!["args".into()] } else { Vec::new() },
            body,
            parent: Some(self.current),
            declaration: false,
            binds_name: false,
            uses_arguments: false,
            pos: end,
            params_pos: end,
            end,
            eager: false,
            strict: true,
            withs: self.withs.clone(),
            arrow: false,
            // (A derived one's `super(...args)`: see `super_expr`.)
            uses_this: derived,
            method: false,
            length: 0,
            rest: derived,
            param_stmts: 0,
            binding: None,
            annex_b: false,
            class_kind,
            has_eval: false,
            generator: false,
            is_async: false,
            uses_new_target: true,
            hidden: Vec::new(),
        text: None,
            home: Some(home.clone()),
            ctor_var: Some(home_static.clone()),
            free: None,
        });
        index
    }

    /// `super(…)`, `super.name` or `super[key]` (at `super`).
    fn super_expr(&mut self, pos: Pos) -> R<Expr<'a>> {
        let k = self.non_arrow();
        if self.is("(") {
            // (Maybe in an arrow in it: the constructor's `this` is then
            // a variable the arrow sets, `%this`, as it is whenever the
            // constructor calls `super()`.)
            let ctor = match &self.functions[k].ctor_var {
                Some(c) if self.functions[k].class_kind == 3 => c.clone(),
                _ => return self.err("`super()` outside a derived class's constructor"),
            };
            self.functions[k].uses_this = true;
            self.advance();
            let args = self.arguments()?;
            return Ok(Expr {
                kind: ExprKind::SuperCall(ctor, args),
                pos,
            });
        }
        let Some(home) = self.functions[k].home.clone() else {
            return self.err("`super` outside a method");
        };
        self.home_uses += 1;
        let key = if self.eat(".") {
            let n = self.property_name()?;
            Expr {
                kind: ExprKind::Str(n.to_string()),
                pos,
            }
        } else if self.eat("[") {
            let e = self.expression(false)?;
            self.expect("]")?;
            e
        } else {
            return self.err("expected `.`, `[` or `(` after `super`");
        };
        let this = if self.functions[self.current].arrow {
            self.functions[k].uses_this = true;
            ExprKind::Ident("%this".into())
        } else {
            ExprKind::This
        };
        let this = expr(self.arena, Expr { kind: this, pos });
        Ok(Expr {
            kind: ExprKind::SuperMember(home, expr(self.arena, key), this),
            pos,
        })
    }

    /// A template literal's pieces from its first (`first`), and its
    /// substitutions.
    #[allow(clippy::type_complexity)]
    fn template_rest(
        &mut self,
        first: Box<super::lex::TemplatePart>,
    ) -> R<(Vec<(Option<String>, String)>, Vec<Expr<'a>>)> {
        let (mut parts, mut subs) = (Vec::new(), Vec::new());
        let mut part = first;
        loop {
            let tail = part.tail;
            let p = *part;
            parts.push((p.cooked, p.raw));
            if tail {
                return Ok((parts, subs));
            }
            subs.push(self.expression(false)?);
            match self.peek() {
                Tok::Template(_) => match self.bump() {
                    Tok::Template(t) => part = t,
                    _ => unreachable!(),
                },
                _ => return self.err("expected the end of a template substitution"),
            }
        }
    }
}

/// A `for … in` head's target other than a name.
enum ForInTarget<'a> {
    Expr(Expr<'a>),
    /// A pattern, `let`/`const` (`Some(constant)`) or not, and the names it
    /// declares (none: an assignment pattern).
    Pattern(Pattern<'a>, Option<bool>, Vec<Name>),
}

/// Rename `p`'s names by `map` (block-scoped bindings' new names).
fn rename_pattern(p: &mut Pattern, map: &[(Name, Name)]) {
    match p {
        Pattern::Name(n) => {
            if let Some((_, r)) = map.iter().find(|(o, _)| o == n) {
                *n = r.clone();
            }
        }
        Pattern::Target(_) => {}
        Pattern::Array(es, rest) => {
            es.iter_mut().flatten().for_each(|(p, _)| rename_pattern(p, map));
            if let Some(r) = rest {
                rename_pattern(r, map);
            }
        }
        Pattern::Object(ps, rest) => {
            ps.iter_mut().for_each(|(_, p, _)| rename_pattern(p, map));
            if let Some(r) = rest {
                rename_pattern(r, map);
            }
        }
    }
}

/// Function `f`'s `var`-scoped names: its parameters, `var`s, the
/// functions declared in it, `arguments`, its own name.
fn var_names(fs: &[Function], f: usize) -> Vec<Name> {
    fn walk(s: &Stmt, fs: &[Function], out: &mut Vec<Name>) {
        match &s.kind {
            StmtKind::Var(ds)
            | StmtKind::For {
                init: Some(ForInit::Var(ds)),
                ..
            } => out.extend(ds.iter().filter(|(n, _)| !n.is_empty()).map(|(n, _)| n.clone())),
            StmtKind::ForIn { var: true, name, .. } => out.push(name.clone()),
            StmtKind::ForOf {
                kind: 1,
                pattern: Some(p),
                ..
            } => p.names(out),
            StmtKind::ForOf {
                kind: 1,
                target: Expr {
                    kind: ExprKind::Ident(name),
                    ..
                },
                ..
            } => out.push(name.clone()),
            StmtKind::Function(i) => out.extend(fs[*i].name.clone()),
            _ => {}
        }
        match &s.kind {
            StmtKind::If(_, a, b) => {
                walk(a, fs, out);
                if let Some(b) = b {
                    walk(b, fs, out);
                }
            }
            StmtKind::For { body, .. }
            | StmtKind::ForIn { body, .. }
            | StmtKind::ForOf { body, .. }
            | StmtKind::While(_, body)
            | StmtKind::DoWhile(body, _)
            | StmtKind::With(_, _, body)
            | StmtKind::Labeled(_, body) => walk(body, fs, out),
            StmtKind::Block(b) => b.iter().for_each(|s| walk(s, fs, out)),
            StmtKind::Switch(_, cases) => cases.iter().flat_map(|(_, b)| b).for_each(|s| walk(s, fs, out)),
            StmtKind::Try(b, c, fin) => b
                .iter()
                .chain(c.iter().flat_map(|(_, c)| c))
                .chain(fin.iter().flatten())
                .for_each(|s| walk(s, fs, out)),
            _ => {}
        }
    }
    let func = &fs[f];
    let mut out: Vec<Name> = func.params.to_vec();
    func.body.iter().for_each(|s| walk(s, fs, &mut out));
    if !func.arrow {
        out.push("arguments".into());
    }
    if func.binds_name {
        out.extend(func.name.clone());
    }
    out
}

fn take<'a>(e: &mut Expr<'a>) -> Expr<'a> {
    std::mem::replace(
        e,
        Expr {
            kind: ExprKind::Null,
            pos: 0,
        },
    )
}

enum Bin {
    Op(BinOp),
    And,
    Or,
}

/// A binary operator token's precedence (higher binds tighter) and meaning.
fn binary_op(t: &Tok) -> Option<(u8, Bin)> {
    use BinOp::*;
    let s = match t {
        Tok::Punct(p) => *p,
        Tok::Keyword(k @ ("in" | "instanceof")) => *k,
        _ => return None,
    };
    Some(match s {
        "||" => (1, Bin::Or),
        "&&" => (2, Bin::And),
        "|" => (3, Bin::Op(BitOr)),
        "^" => (4, Bin::Op(BitXor)),
        "&" => (5, Bin::Op(BitAnd)),
        "==" => (6, Bin::Op(Eq)),
        "!=" => (6, Bin::Op(Ne)),
        "===" => (6, Bin::Op(StrictEq)),
        "!==" => (6, Bin::Op(StrictNe)),
        "<" => (7, Bin::Op(Lt)),
        ">" => (7, Bin::Op(Gt)),
        "<=" => (7, Bin::Op(Le)),
        ">=" => (7, Bin::Op(Ge)),
        "in" => (7, Bin::Op(In)),
        "instanceof" => (7, Bin::Op(Instanceof)),
        "<<" => (8, Bin::Op(Shl)),
        ">>" => (8, Bin::Op(Sar)),
        ">>>" => (8, Bin::Op(Shr)),
        "+" => (9, Bin::Op(Add)),
        "-" => (9, Bin::Op(Sub)),
        "*" => (10, Bin::Op(Mul)),
        "/" => (10, Bin::Op(Div)),
        "%" => (10, Bin::Op(Mod)),
        "**" => (11, Bin::Op(Exp)),
        _ => return None,
    })
}

/// The operator of a compound assignment token (`+=` → `+`).
fn compound(p: &str) -> Option<BinOp> {
    use BinOp::*;
    Some(match p {
        "+=" => Add,
        "-=" => Sub,
        "*=" => Mul,
        "/=" => Div,
        "%=" => Mod,
        "**=" => Exp,
        "<<=" => Shl,
        ">>=" => Sar,
        ">>>=" => Shr,
        "&=" => BitAnd,
        "|=" => BitOr,
        "^=" => BitXor,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::super::lex::lex;
    use super::*;

    /// (Its nodes in an arena that lives on: these are tests.)
    fn program(src: &str) -> Program<'static> {
        parse(&mut lex(src).unwrap(), Box::leak(Box::default())).unwrap()
    }

    fn error(src: &str) -> String {
        parse(&mut lex(src).unwrap(), &Arenas::default()).unwrap_err().msg
    }

    /// The script's only expression statement, printed with explicit
    /// grouping.
    fn show(src: &str) -> String {
        fn s(e: &Expr) -> String {
            match &e.kind {
                ExprKind::Num(v) => format!("{v}"),
                ExprKind::Ident(n) => n.to_string(),
                ExprKind::Binary(op, l, r) => format!("({} {} {})", s(l), op.symbol(), s(r)),
                ExprKind::Logical(and, l, r) => {
                    format!("({} {} {})", s(l), if *and { "&&" } else { "||" }, s(r))
                }
                ExprKind::Assign(op, l, r) => {
                    format!("({} {}= {})", s(l), op.map_or("", |o| o.symbol()), s(r))
                }
                ExprKind::Cond(c, a, b) => format!("({} ? {} : {})", s(c), s(a), s(b)),
                ExprKind::Unary(op, e) => format!("({op:?} {})", s(e)),
                ExprKind::Update(e, d, pre) => {
                    format!("(upd{d}{} {})", if *pre { "pre" } else { "post" }, s(e))
                }
                ExprKind::Member(o, n) => format!("{}.{n}", s(o)),
                ExprKind::Index(o, k) => format!("{}[{}]", s(o), s(k)),
                ExprKind::Call(f, a) => format!(
                    "{}({})",
                    s(f),
                    a.iter().map(s).collect::<Vec<_>>().join(", ")
                ),
                ExprKind::New(f, a) => format!(
                    "new {}({})",
                    s(f),
                    a.iter().map(s).collect::<Vec<_>>().join(", ")
                ),
                ExprKind::Comma(a, b) => format!("({}, {})", s(a), s(b)),
                k => format!("{k:?}"),
            }
        }
        let p = program(src);
        match &p.functions[0].body[..] {
            [
                Stmt {
                    kind: StmtKind::Expr(e),
                    ..
                },
            ] => s(e),
            b => panic!("{b:?}"),
        }
    }

    #[test]
    fn binds_operators_by_precedence_and_associativity() {
        assert_eq!(show("a + b * c - d"), "((a + (b * c)) - d)");
        assert_eq!(
            show("a = b += c ? d : e || f && g"),
            "(a = (b += (c ? d : (e || (f && g)))))"
        );
        assert_eq!(
            show("a < b == c < d & e | f ^ g"),
            "((((a < b) == (c < d)) & e) | (f ^ g))"
        );
        assert_eq!(show("a ** b ** c"), "(a ** (b ** c))");
        assert_eq!(show("-a.b++ + !c"), "((Neg (upd1post a.b)) + (Not c))");
        assert_eq!(show("a, b = c"), "(a, (b = c))");
    }

    #[test]
    fn parses_calls_members_and_new() {
        assert_eq!(show("a.b[c](d, e).f"), "a.b[c](d, e).f");
        assert_eq!(show("new a.B(c).d"), "new a.B(c).d");
        assert_eq!(show("new new A()()"), "new new A()()");
        assert_eq!(show("new A"), "new A()");
        assert_eq!(show("a.default"), "a.default");
    }

    #[test]
    fn inserts_semicolons_at_line_breaks() {
        let p = program("a = 1\nb = 2\nreturn_ = a\n++b");
        assert_eq!(p.functions[0].body.len(), 4);
        let p = program("function f() { return\n1 }");
        assert!(matches!(
            p.functions[1].body[0].kind,
            StmtKind::Return(None)
        ));
        assert!(error("a b").contains("unexpected `b`"));
    }

    #[test]
    fn collects_functions_with_parents_and_inferred_names() {
        let p = program(
            "function f(a, b) { function g() {} var h = function () {}; }\n\
             C.prototype.m = function () { return { k: function () {} }; };",
        );
        let names: Vec<_> = p.functions.iter().map(|f| f.name.as_deref()).collect();
        assert_eq!(
            names,
            [None, Some("f"), Some("g"), Some("h"), Some("m"), Some("k")]
        );
        let parents: Vec<_> = p.functions.iter().map(|f| f.parent).collect();
        assert_eq!(parents, [None, Some(0), Some(1), Some(1), Some(0), Some(4)]);
        assert_eq!(p.functions[1].params, ["a".into(), "b".into()]);
        assert!(p.functions[2].declaration && !p.functions[3].declaration);
    }

    #[test]
    fn separates_for_in_from_for() {
        let p = program(
            "for (var i = 0, n = ('a' in o); i < 1; i++) ; for (var p in o) ; for (q in o) ;",
        );
        let b = &p.functions[0].body;
        assert!(
            matches!(&b[0].kind, StmtKind::For { init: Some(ForInit::Var(d)), .. } if d.len() == 2)
        );
        assert!(matches!(&b[1].kind, StmtKind::ForIn { var: true, name, .. } if &**name == "p"));
        assert!(matches!(&b[2].kind, StmtKind::ForIn { var: false, .. }));
    }

    #[test]
    fn reports_errors() {
        assert!(error("import x from \"y\";").contains("not supported"));
        assert!(error("f(").contains("end of input"));
        assert!(error("1 = 2").contains("assignment target"));
        assert!(error("return 1").contains("outside a function"));
        assert!(error("function (a) {}").contains("name"));
    }
}
