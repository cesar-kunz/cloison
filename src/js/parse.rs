//! The parser: tokens into a [`Program`], by recursive descent, with
//! precedence climbing for binary operators.
//!
//! It covers ES5's statements and expressions minus `with`,
//! getters and setters and regular expressions.

use super::Error;
use super::ast::*;
use super::lex::{Tok, Token};

pub fn parse(toks: &[Token]) -> Result<Program, Error> {
    let mut p = Parser {
        toks,
        i: 0,
        functions: Vec::new(),
        current: 0,
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
    });
    let mut body = Vec::new();
    while p.peek() != &Tok::Eof {
        body.push(p.statement()?);
    }
    p.functions[0].body = body;
    Ok(Program {
        functions: p.functions,
    })
}

struct Parser<'a> {
    toks: &'a [Token],
    i: usize,
    functions: Vec<Function>,
    /// The function whose body is being parsed.
    current: usize,
}

type R<T> = Result<T, Error>;

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.toks[self.i].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.i + n).min(self.toks.len() - 1)].tok
    }

    fn pos(&self) -> Pos {
        self.toks[self.i].pos
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.i].tok.clone();
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

    fn statement(&mut self) -> R<Stmt> {
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
            Tok::Keyword(k @ ("let" | "const" | "class" | "with" | "import" | "export")) => {
                return self.err(format!("`{k}` is not supported"));
            }
            Tok::Keyword("function") => {
                self.advance();
                StmtKind::Function(self.function(true, pos)?)
            }
            Tok::Keyword("if") => {
                self.advance();
                self.expect("(")?;
                let c = self.expression(false)?;
                self.expect(")")?;
                let then = Box::new(self.statement()?);
                let other = if self.eat("else") {
                    Some(Box::new(self.statement()?))
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
                StmtKind::While(c, Box::new(self.statement()?))
            }
            Tok::Keyword("do") => {
                self.advance();
                let body = Box::new(self.statement()?);
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
                StmtKind::Labeled(name, Box::new(self.statement()?))
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
    fn block_rest(&mut self) -> R<Vec<Stmt>> {
        let mut body = Vec::new();
        while !self.eat("}") {
            if *self.peek() == Tok::Eof {
                return self.err("expected `}` before end of input");
            }
            body.push(self.statement()?);
        }
        Ok(body)
    }

    fn var_decls(&mut self, no_in: bool) -> R<Vec<(Name, Option<Expr>)>> {
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

    fn for_rest(&mut self) -> R<StmtKind> {
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
        let body = Box::new(self.statement()?);
        Ok(StmtKind::For {
            init,
            test,
            update,
            body,
        })
    }

    fn for_in_rest(&mut self, var: bool, name: Name) -> R<StmtKind> {
        let object = self.expression(false)?;
        self.expect(")")?;
        let body = Box::new(self.statement()?);
        Ok(StmtKind::ForIn {
            var,
            name,
            object,
            body,
        })
    }

    fn switch_rest(&mut self) -> R<StmtKind> {
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
    /// and body. Returns the function's index.
    fn function(&mut self, declaration: bool, pos: Pos) -> R<usize> {
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
        });
        self.expect("(")?;
        let mut params = Vec::new();
        while !self.eat(")") {
            if !params.is_empty() {
                self.expect(",")?;
            }
            let p = self.ident()?;
            if params.contains(&p) {
                return self.err(format!("duplicate parameter `{p}`"));
            }
            params.push(p);
        }
        self.expect("{")?;
        let outer = std::mem::replace(&mut self.current, index);
        let body = self.block_rest();
        self.current = outer;
        let f = &mut self.functions[index];
        f.params = params;
        f.body = body?;
        Ok(index)
    }

    /// Name an anonymous function expression after what it's assigned to.
    fn infer_name(&mut self, e: &Expr, name: &str) {
        if let ExprKind::Function(i) = e.kind
            && self.functions[i].name.is_none()
        {
            self.functions[i].name = Some(name.into());
        }
    }

    // ---- Expressions ----

    /// `no_in` excludes the `in` operator (in a `for` head).
    fn expression(&mut self, no_in: bool) -> R<Expr> {
        let mut e = self.assignment(no_in)?;
        while self.is(",") {
            let pos = self.pos();
            self.advance();
            let r = self.assignment(no_in)?;
            e = Expr {
                kind: ExprKind::Comma(Box::new(e), Box::new(r)),
                pos,
            };
        }
        Ok(e)
    }

    fn assignment(&mut self, no_in: bool) -> R<Expr> {
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
            kind: ExprKind::Assign(op, Box::new(target), Box::new(value)),
            pos,
        })
    }

    fn conditional(&mut self, no_in: bool) -> R<Expr> {
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
            kind: ExprKind::Cond(Box::new(c), Box::new(a), Box::new(b)),
            pos,
        })
    }

    /// Binary operators binding tighter than `min`.
    fn binary(&mut self, min: u8, no_in: bool) -> R<Expr> {
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
                Bin::Op(op) => ExprKind::Binary(op, Box::new(l), Box::new(r)),
                Bin::And => ExprKind::Logical(true, Box::new(l), Box::new(r)),
                Bin::Or => ExprKind::Logical(false, Box::new(l), Box::new(r)),
            };
            l = Expr { kind, pos };
        }
    }

    fn unary(&mut self) -> R<Expr> {
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
                    kind: ExprKind::Update(Box::new(target), d, true),
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
            kind: ExprKind::Unary(op, Box::new(e)),
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

    fn postfix(&mut self) -> R<Expr> {
        let e = self.call()?;
        if let Tok::Punct(p @ ("++" | "--")) = self.peek()
            && !self.toks[self.i].newline_before
        {
            let d = if *p == "++" { 1 } else { -1 };
            let pos = self.pos();
            self.check_update_target(&e)?;
            self.advance();
            return Ok(Expr {
                kind: ExprKind::Update(Box::new(e), d, false),
                pos,
            });
        }
        Ok(e)
    }

    /// Member accesses and calls.
    fn call(&mut self) -> R<Expr> {
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
                    kind: ExprKind::Call(Box::new(e), args),
                    pos,
                };
            } else if !self.member_suffix(&mut e)? {
                return Ok(e);
            }
        }
    }

    /// `.name` or `[key]` applied to `e`, if one follows.
    fn member_suffix(&mut self, e: &mut Expr) -> R<bool> {
        let pos = self.pos();
        let kind = if self.eat(".") {
            let name = self.property_name()?;
            ExprKind::Member(Box::new(take(e)), name)
        } else if self.eat("[") {
            let k = self.expression(false)?;
            self.expect("]")?;
            ExprKind::Index(Box::new(take(e)), Box::new(k))
        } else {
            return Ok(false);
        };
        *e = Expr { kind, pos };
        Ok(true)
    }

    /// `new C(args)`, where `C` is a member expression without calls, and
    /// the arguments are optional.
    fn new_expr(&mut self) -> R<Expr> {
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
            kind: ExprKind::New(Box::new(c), args),
            pos,
        })
    }

    /// After `(`: arguments up to and including `)`.
    fn arguments(&mut self) -> R<Vec<Expr>> {
        let mut args = Vec::new();
        while !self.eat(")") {
            if !args.is_empty() {
                self.expect(",")?;
            }
            args.push(self.assignment(false)?);
        }
        Ok(args)
    }

    fn primary(&mut self) -> R<Expr> {
        let pos = self.pos();
        let start = self.i;
        let kind = match self.bump() {
            Tok::Num(v) => ExprKind::Num(v),
            Tok::Str(s) => ExprKind::Str(s),
            Tok::Ident(s) => {
                if &*s == "arguments" {
                    self.functions[self.current].uses_arguments = true;
                }
                ExprKind::Ident(s)
            }
            Tok::Keyword("this") => ExprKind::This,
            Tok::Keyword("true") => ExprKind::Bool(true),
            Tok::Keyword("false") => ExprKind::Bool(false),
            Tok::Keyword("null") => ExprKind::Null,
            Tok::Keyword("function") => ExprKind::Function(self.function(false, pos)?),
            Tok::Punct("(") => {
                let e = self.expression(false)?;
                self.expect(")")?;
                return Ok(e);
            }
            Tok::Punct("[") => {
                let mut elems = Vec::new();
                while !self.eat("]") {
                    if self.is(",") {
                        return self.err("array holes are not supported");
                    }
                    elems.push(self.assignment(false)?);
                    if !self.is("]") {
                        self.expect(",")?;
                    }
                }
                ExprKind::Array(elems)
            }
            Tok::Punct("{") => {
                let mut props: Vec<(Name, Expr)> = Vec::new();
                while !self.eat("}") {
                    let key_start = self.i;
                    let key: Name = match self.bump() {
                        Tok::Ident(s) => s,
                        Tok::Str(s) => s.into(),
                        Tok::Keyword(k) => k.into(),
                        Tok::Num(v) => super::number_to_string(v).into(),
                        _ => {
                            self.i = key_start;
                            return self.err("expected a property name");
                        }
                    };
                    // `get k() {…}` / `set k(v) {…}`.
                    if matches!(&*key, "get" | "set") && !self.is(":") {
                        let name_at = self.i;
                        let name: Name = match self.bump() {
                            Tok::Ident(s) => s,
                            Tok::Str(s) => s.into(),
                            Tok::Keyword(k) => k.into(),
                            Tok::Num(v) => super::number_to_string(v).into(),
                            _ => {
                                self.i = name_at;
                                return self.err("expected a property name");
                            }
                        };
                        let f = self.function(false, pos)?;
                        let kind = ExprKind::Accessor(&*key == "set", f);
                        props.push((name, Expr { kind, pos }));
                        if !self.is("}") {
                            self.expect(",")?;
                        }
                        continue;
                    }
                    self.expect(":")?;
                    let v = self.assignment(false)?;
                    self.infer_name(&v, &key);
                    props.push((key, v));
                    if !self.is("}") {
                        self.expect(",")?;
                    }
                }
                ExprKind::Object(props)
            }
            Tok::RegExp(p, f) => ExprKind::RegExp(p, f),
            _ => {
                self.i = start;
                return self.unexpected();
            }
        };
        Ok(Expr { kind, pos })
    }
}

fn take(e: &mut Expr) -> Expr {
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

    fn program(src: &str) -> Program {
        parse(&lex(src).unwrap()).unwrap()
    }

    fn error(src: &str) -> String {
        parse(&lex(src).unwrap()).unwrap_err().msg
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
