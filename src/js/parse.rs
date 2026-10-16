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
        free: None,
    });
    p.functions[0].strict = p.directive_prologue();
    let mut body = Vec::new();
    while p.peek() != &Tok::Eof {
        body.push(p.statement()?);
    }
    p.functions[0].body = body;
    Ok(Program {
        functions: p.functions,
    })
}

struct Parser<'t, 'a> {
    toks: &'t mut [Token],
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

impl<'a> Parser<'_, 'a> {
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
            Tok::Str(s) => Tok::Str(std::mem::take(s)),
            Tok::RegExp(r) => Tok::RegExp(std::mem::take(r)),
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
                Tok::Ident(s) => Ok(s),
                _ => unreachable!(),
            },
            _ => self.err("expected an identifier"),
        }
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
            if s == "use strict" {
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
        let kind = match self.peek().clone() {
            Tok::Punct("{") => {
                self.advance();
                StmtKind::Block(self.block_rest()?)
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
            Tok::Keyword(k @ ("const" | "class" | "import" | "export")) => {
                return self.err(format!("`{k}` is not supported"));
            }
            Tok::Ident(n) if &*n == "let" && !self.toks[self.i].escaped && matches!(self.peek_at(1), Tok::Ident(_) | Tok::Punct("[" | "{")) => {
                return self.err("`let` is not supported");
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
                let body = self.statement();
                self.withs.pop();
                StmtKind::With(o, name, stmt(self.arena, body?))
            }
            Tok::Keyword("function") => {
                self.advance();
                StmtKind::Function(self.function(true, false, pos)?)
            }
            Tok::Keyword("if") => {
                self.advance();
                self.expect("(")?;
                let c = self.expression(false)?;
                self.expect(")")?;
                let then = stmt(self.arena, self.statement()?);
                let other = if self.eat("else") {
                    Some(stmt(self.arena, self.statement()?))
                } else {
                    None
                };
                StmtKind::If(c, then, other)
            }
            Tok::Keyword("for") => {
                self.advance();
                self.for_rest()?
            }
            Tok::Keyword("while") => {
                self.advance();
                self.expect("(")?;
                let c = self.expression(false)?;
                self.expect(")")?;
                StmtKind::While(c, stmt(self.arena, self.statement()?))
            }
            Tok::Keyword("do") => {
                self.advance();
                let body = stmt(self.arena, self.statement()?);
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
                let block = self.block_rest()?;
                let handler = if self.eat("catch") {
                    self.expect("(")?;
                    let name = self.ident()?;
                    self.expect(")")?;
                    self.expect("{")?;
                    Some((name, self.block_rest()?))
                } else {
                    None
                };
                let finalizer = if self.eat("finally") {
                    self.expect("{")?;
                    Some(self.block_rest()?)
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
                StmtKind::Labeled(name, stmt(self.arena, self.statement()?))
            }
            _ => {
                let e = self.expression(false)?;
                self.semicolon()?;
                StmtKind::Expr(e)
            }
        };
        Ok(Stmt { kind, pos })
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
            let name = self.ident()?;
            let init = if self.eat("=") {
                let e = self.assignment(no_in)?;
                self.infer_name(&e, &name);
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
        } else if self.eat("var") {
            let decls = self.var_decls(true)?;
            if decls.len() == 1 && decls[0].1.is_none() && self.eat("in") {
                let name = decls.into_iter().next().unwrap().0;
                return self.for_in_rest(true, name);
            }
            Some(ForInit::Var(decls))
        } else {
            let e = self.expression(true)?;
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
        let body = stmt(self.arena, self.statement()?);
        Ok(StmtKind::For {
            init,
            test,
            update,
            body,
        })
    }

    fn for_in_rest(&mut self, var: bool, name: Name) -> R<StmtKind<'a>> {
        let object = self.expression(false)?;
        self.expect(")")?;
        let body = stmt(self.arena, self.statement()?);
        Ok(StmtKind::ForIn {
            var,
            name,
            object,
            body,
        })
    }

    fn switch_rest(&mut self) -> R<StmtKind<'a>> {
        self.expect("(")?;
        let d = self.expression(false)?;
        self.expect(")")?;
        self.expect("{")?;
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
        Ok(StmtKind::Switch(d, cases))
    }

    /// After `function`: the name (if `declaration`, required), parameters
    /// and body. Returns the function's index. A body is only skimmed when
    /// skimming, unless the function comes right after a `(` (`paren`) or
    /// right before one.
    fn function(&mut self, declaration: bool, paren: bool, pos: Pos) -> R<usize> {
        let name = match self.peek() {
            Tok::Ident(_) => Some(self.ident()?),
            _ if declaration => return self.err("expected the function's name"),
            _ => None,
        };
        let index = self.functions.len();
        let binds_name = !declaration && name.is_some();
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
            free: None,
        });
        self.expect("(")?;
        let outer = std::mem::replace(&mut self.current, index);
        let params = self.params();
        self.current = outer;
        let (params, prologue) = params?;
        self.expect("{")?;
        if self.directive_prologue() {
            self.functions[index].strict = true;
        }
        if self.functions[index].strict {
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
            {
                let end = self.toks[self.i - 1].pos + 1;
                let f = &mut self.functions[index];
                f.end = end;
                f.params = params;
                f.free = Some(free);
                return Ok(index);
            }
            self.i = start;
        }
        let outer = std::mem::replace(&mut self.current, index);
        let body = self.block_rest();
        self.current = outer;
        let end = self.toks[self.i - 1].pos + 1;
        let f = &mut self.functions[index];
        f.end = end;
        f.params = params;
        let mut body = body?;
        if !prologue.is_empty() {
            body.splice(0..0, prologue);
        }
        f.body = body;
        Ok(index)
    }

    /// After a function's `(`: its parameters, up to and including the
    /// `)`, and the statements giving those with defaults their values
    /// (`if (p === void 0) p = e;`). Sets the function's `length` and
    /// `rest`; it's the current one.
    fn params(&mut self) -> R<(Vec<Name>, Vec<Stmt<'a>>)> {
        let mut params: Vec<Name> = Vec::new();
        let mut prologue = Vec::new();
        let mut length = None;
        while !self.eat(")") {
            if !params.is_empty() {
                self.expect(",")?;
                if self.eat(")") {
                    break;
                }
            }
            if self.eat("...") {
                let p = self.ident()?;
                length.get_or_insert(params.len());
                params.push(p);
                self.functions[self.current].rest = true;
                self.expect(")")?;
                break;
            }
            let pos = self.pos();
            let p = self.ident()?;
            if self.eat("=") {
                length.get_or_insert(params.len());
                let e = self.assignment(false)?;
                self.infer_name(&e, &p);
                prologue.push(self.default_stmt(p.clone(), e, pos));
            }
            params.push(p);
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

    /// After a `(`: whether `=>` follows its matching `)`.
    fn arrow_ahead(&self) -> bool {
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
            self.block_rest()
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
        if let ExprKind::Function(i) = e.kind
            && self.functions[i].name.is_none()
        {
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
        if !matches!(
            target.kind,
            ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..)
        ) {
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

    fn check_update_target(&self, e: &Expr) -> R<()> {
        match e.kind {
            ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..) => Ok(()),
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
        let mut c = if self.is("new") {
            self.new_expr()?
        } else {
            self.primary()?
        };
        while self.member_suffix(&mut c)? {}
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
            Tok::Ident(s) => self.reference(s),
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
                    if let Some(set) = accessor {
                        self.advance();
                        let key = self.prop_key()?;
                        let prefix = if set { "set" } else { "get" };
                        let f = self.method(&key, Some(prefix), ppos)?;
                        props.push((key, Expr { kind: ExprKind::Accessor(set, f), pos: ppos }));
                    } else {
                        let shorthand = match self.peek() {
                            Tok::Ident(n) if matches!(self.peek_at(1), Tok::Punct("," | "}")) => Some(n.clone()),
                            _ => None,
                        };
                        if let Some(n) = shorthand {
                            self.advance();
                            let kind = self.reference(n.clone());
                            props.push((PropKey::Name(n), Expr { kind, pos: ppos }));
                        } else {
                            let key = self.prop_key()?;
                            if self.is("(") {
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
                ExprKind::Object(props)
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

impl<'a> Parser<'_, 'a> {
    /// A reference to the variable `name`.
    fn reference(&mut self, name: Name) -> ExprKind<'a> {
        if &*name == "arguments" {
            // (An arrow's are the enclosing function's.)
            let k = self.non_arrow();
            self.functions[k].uses_arguments = true;
        }
        ExprKind::Ident(name)
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
        assert!(error("let x = 1;").contains("not supported"));
        assert!(error("f(").contains("end of input"));
        assert!(error("1 = 2").contains("assignment target"));
        assert!(error("return 1").contains("outside a function"));
        assert!(error("function (a) {}").contains("name"));
    }
}
