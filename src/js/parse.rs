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
/// A statement list that may have `using` declarations (see
/// `Parser::using_scopes`): whether it may, and its disposal's hidden
/// variables once one is found, and whether one is `await using`.
struct UsingScope {
    allowed: bool,
    names: Option<UsingNames>,
    is_await: bool,
}

/// The hidden variables of a statement list's disposal (see
/// `Parser::lower_using`).
#[derive(Clone)]
struct UsingNames {
    state: Name,
    has: Name,
    error: Name,
    caught: Name,
    pending: Name,
}

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
    let (first, table) = parse_pass(toks, arena, skim && !lexical, true, None, None, None, None)?;
    if table.trivial() {
        return Ok(first);
    }
    parse_pass(toks, arena, skim, false, Some(table), Some(&first), None, None).map(|(p, _)| p)
}

/// [`parse_with`] of source `src`, lexed again for a second pass (the
/// first takes the strings out of its tokens).
pub fn parse_source<'a>(src: &str, arena: &'a Arenas<'a>, skim: bool) -> Result<Program<'a>, Error> {
    parse_source_as(src, arena, skim, None)
}

thread_local! {
    /// Scripts parsed are strict code (see `js::compile_strict_eval`).
    pub(crate) static STRICT: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// [`parse_source`]; `eval`: as direct `eval`'s code (strict or not, and
/// in that many `with` statements).
pub fn parse_source_as<'a>(src: &str, arena: &'a Arenas<'a>, skim: bool, eval: Option<(bool, usize, bool)>) -> Result<Program<'a>, Error> {
    let mut toks = super::lex::lex(src)?;
    let lexical = has_lexical(&toks);
    let (first, table) = parse_pass(&mut toks, arena, skim && !lexical, false, None, None, eval, None)?;
    if table.trivial() {
        return Ok(first);
    }
    let mut toks = super::lex::lex(src)?;
    parse_pass(&mut toks, arena, skim, false, Some(table), Some(&first), eval, None).map(|(p, _)| p)
}

/// Module `src`: a script of one function expression, the module's (see
/// [`Parser::module_body`]), with the module's entries
/// ([`Program::module`]). `id`: the module's (see `module`), which its
/// `import()`s and `import.meta` name.
pub fn parse_module<'a>(src: &str, arena: &'a Arenas<'a>, id: i32) -> Result<Program<'a>, Error> {
    let mut toks = super::lex::lex_as(src, true)?;
    let (first, table) = parse_pass(&mut toks, arena, false, false, None, None, None, Some(id))?;
    if table.trivial() {
        return Ok(first);
    }
    let mut toks = super::lex::lex_as(src, true)?;
    parse_pass(&mut toks, arena, false, false, Some(table), Some(&first), None, Some(id)).map(|(p, _)| p)
}

/// Whether `toks` has `let`, `const` or `class` (then a first pass parses
/// every function in full, for their `var`s: see [`Parser::push_vars`]),
/// as the lexer marks its last token.
fn has_lexical(toks: &[Token]) -> bool {
    toks.last().is_some_and(|t| t.escaped)
}

/// One pass of [`parse_with`]: the first collects its [`Table`], the
/// second (`table` given) renames by it.
fn parse_pass<'a>(
    toks: &mut [Token],
    arena: &'a Arenas<'a>,
    skim: bool,
    keep: bool,
    table: Option<Table>,
    prev: Option<&Program<'a>>,
    eval: Option<(bool, usize, bool)>,
    module: Option<i32>,
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
        pattern_names: Vec::new(),
        privates: Vec::new(),
        super_fns: Vec::new(),
        redeclared: None,
        in_eval: eval.is_some(),
        using_scopes: Vec::new(),
        param_refs: Vec::new(),
        static_blocks: Vec::new(),
        naming_function: false,
        param_names: Vec::new(),
        home_uses: 0,
        pending_home: None,
        keep,
        prev,
        table: table.unwrap_or_default(),
        collect: first,
        frames: Vec::new(),
        renames: 0,
        lexicals: Default::default(),
        body_vars: Default::default(),
        shadowed_params: Default::default(),
        module,
        info: ModuleInfo::default(),
        imports: Default::default(),
        local_exports: Vec::new(),
        exported: Vec::new(),
        labels: Vec::new(),
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
        param_ends: Vec::new(),
        tdz_params: Vec::new(),
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
        fields: None,
        module: false,
    });
    p.functions[0].strict =
        p.directive_prologue() || eval.is_some_and(|(strict, ..)| strict) || module.is_some() || STRICT.get();
    if let Some((_, withs, bound)) = eval {
        // The scope object of the function calling it (its parameters and
        // `var`s, those `eval`s declared), the caller's scope object (the
        // other bindings it sees: see `intrinsics::eval_scope`), the
        // objects of the `with` statements around the `eval` (outermost
        // first), then the eval's own (for strict code's `var`s): what
        // `with`s would give. (`bound`: no scope object of the caller's,
        // its bindings its upvalues: see `Program::eval_upvalues`; nor, for
        // sloppy code, of its own, which would stay empty: no `eval` in it
        // to declare there.)
        let names: Vec<Name> = std::iter::once("%e2".into())
            .chain((!bound).then(|| "%e0".into()))
            .chain((0..withs).map(|k| format!("%w{k}").into()))
            .chain((!bound || p.functions[0].strict).then(|| "%e1".into()))
            .collect();
        for e in &names {
            p.hidden_var(e.clone());
            p.lexicals.get_mut(e).unwrap().withs = names.len();
            p.withs.push(e.clone());
            p.functions[0].withs.push(e.clone());
        }
    }
    p.push_scope(usize::MAX, false, true);
    let mut body = Vec::new();
    if module.is_some() {
        body.push(p.module_body()?);
    }
    while p.peek() != &Tok::Eof {
        body.push(p.statement()?);
    }
    p.frames.pop();
    if let Some((pos, msg)) = p.redeclared.take() {
        return Err(Error::new(pos as usize, msg));
    }
    p.functions[0].body = body;
    Ok((
        Program {
            functions: p.functions,
            lexicals: p.lexicals,
            body_vars: std::mem::take(&mut p.body_vars),
            shadowed_params: std::mem::take(&mut p.shadowed_params),
            direct_eval: eval.is_some(),
            eval_locals: false,
            eval_upvalues: Vec::new(),
            own_names: Vec::new(),
            imports: std::mem::take(&mut p.imports),
            module: module.map(|_| std::mem::take(&mut p.info)),
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
/// An imported binding (see `ast::Import`): a constant, initialized
/// before the module runs (reading one before its exporter sets it is a
/// ReferenceError, which the reference checks: see `codegen`).
const IMPORT: u8 = 5;

/// The key of a module's own scope (see [`Parser::module_body`]).
const MODULE_SCOPE: usize = usize::MAX - 2;

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
    /// The names the last parameter list's patterns bind.
    pattern_names: Vec<Name>,
    /// The private names of the classes being parsed (innermost last),
    /// each with its variable.
    privates: Vec<Vec<(Name, Name)>>,
    /// The functions with a `super(…)` call, with the variable holding
    /// their class (see `Function::fields`).
    super_fns: Vec<(Name, usize)>,
    /// Each function's parameters' names (destructured ones' too).
    param_names: Vec<(usize, Vec<Name>)>,
    /// Direct `eval`'s code (whose `new.target` is its caller's).
    in_eval: bool,
    /// The statement lists being parsed that may have `using` declarations
    /// (a block's, a function body's: not a `case`'s, nor the script's,
    /// which have none here), and what their disposal needs.
    using_scopes: Vec<UsingScope>,
    /// The parameter lists being parsed (innermost last): the names each
    /// refers to, and where (see `Function::tdz_params`).
    param_refs: Vec<Vec<(Name, Pos)>>,
    /// The functions that are class static blocks, where `await` is no
    /// identifier.
    static_blocks: Vec<usize>,
    /// Whether the identifier being read is a function expression's name.
    naming_function: bool,
    /// A redeclaration found (an early error, reported once parsed).
    redeclared: Option<(Pos, String)>,
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
    /// (See `Program::body_vars`.)
    body_vars: std::collections::HashMap<Name, Name>,
    /// (See `Program::shadowed_params`.)
    shadowed_params: std::collections::HashSet<(usize, Name)>,
    /// Parsing a module: its id (see [`parse_module`]).
    module: Option<i32>,
    /// A module's entries so far (see [`Program::module`]).
    info: ModuleInfo,
    imports: std::collections::HashMap<Name, Import>,
    /// `export { x as y }`: the name exported, the variable (as written,
    /// or renamed), where; checked declared once the module is parsed.
    local_exports: Vec<(String, Name, Pos)>,
    /// The names a module exports so far (each once: an early error).
    exported: Vec<String>,
    /// The labels of the statements being parsed, each with its function.
    labels: Vec<(usize, Name)>,
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

    /// An error where nesting has used up the native stack (see
    /// `crate::stack`): called by the functions every level of nesting goes
    /// through (statements, expressions, patterns).
    fn deep(&self) -> R<()> {
        if crate::stack::low() {
            return self.err("too deeply nested");
        }
        Ok(())
    }

    fn unexpected<T>(&self) -> R<T> {
        let what = match self.peek() {
            Tok::Ident(s) => format!("`{s}`"),
            Tok::Keyword(k) | Tok::Punct(k) => format!("`{k}`"),
            Tok::Num(_) | Tok::BigInt(_) => "a number".into(),
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

    /// A declaration's name: not `eval` or `arguments` in strict code.
    fn binding_ident(&mut self) -> R<Name> {
        let n = self.ident()?;
        if self.strict() && matches!(&*n, "eval" | "arguments") {
            return Err(Error::new(self.toks[self.i - 1].pos as usize, format!("`{n}` declared in strict mode")));
        }
        Ok(n)
    }

    /// An error for a word strict code reserves (just read), if it is one.
    fn strict_word(&self, s: &str) -> R<()> {
        if self.strict()
            && matches!(s, "implements" | "interface" | "package" | "private" | "protected" | "public" | "static")
        {
            return Err(Error::new(self.toks[self.i - 1].pos as usize, format!("`{s}` is reserved in strict mode")));
        }
        // (Escaped, these reach here as identifiers.)
        if self.toks[self.i - 1].escaped && is_reserved_word(s) {
            return Err(Error::new(self.toks[self.i - 1].pos as usize, format!("`{s}` (escaped) is a reserved word")));
        }
        let f = &self.functions[self.current];
        if s == "yield" && (self.strict() || f.generator) || s == "await" && (f.is_async || self.in_static_block() || self.module.is_some()) {
            return Err(Error::new(self.toks[self.i - 1].pos as usize, format!("`{s}` as an identifier here")));
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
            if &**s == "use strict" && !self.toks[j].escaped {
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
        self.deep()?;
        let pos = self.pos();
        let sub = std::mem::replace(&mut self.sub, false);
        // A statement's body (`if`, a loop's, a label's) is no declaration
        // (but in sloppy code a plain function's, Annex B).
        if sub {
            let declaration = match self.peek() {
                Tok::Keyword("const" | "class") => true,
                Tok::Ident(n) if self.let_ahead(n) => !self.toks[self.i + 1].newline_before,
                Tok::Ident(n) if self.async_function_ahead(n) => true,
                Tok::Keyword("function") => self.strict() || self.peek_at(1) == &Tok::Punct("*"),
                _ => false,
            };
            if declaration {
                return self.err("a declaration as a statement's body");
            }
        }
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
            Tok::Ident(_) if !sub && self.using_ahead(false) => self.using_declaration(false)?,
            Tok::Ident(_) if !sub && self.using_ahead(true) => self.using_declaration(true)?,
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
            // (`import(…)` and `import.meta` are expressions.)
            Tok::Keyword(k @ ("import" | "export")) if k == "export" || !matches!(self.peek_at(1), Tok::Punct("(" | ".")) => {
                return self.err(format!("`{k}` declaration not at a module's top level"));
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
                // (A variable of the function's own, also a script's: not a
                // property of the global object.)
                self.hidden_var(name.clone());
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
                } else {
                    self.body_function(f);
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
                // `for await (x of it)`, in an async function.
                let is_await = self.functions[self.current].is_async
                    && matches!(self.peek(), Tok::Ident(n) if &**n == "await")
                    && !self.toks[self.i].escaped;
                if is_await {
                    self.advance();
                    self.note_await();
                }
                let key = self.i;
                self.push_scope(key, false, false);
                let r = self.for_rest();
                self.frames.pop();
                match r? {
                    StmtKind::ForOf { kind, target, pattern, iterable, body, .. } if is_await => {
                        StmtKind::ForOf { kind, target, pattern, iterable, body, is_await }
                    }
                    _ if is_await => return self.err("`for await` without `of`"),
                    r => r,
                }
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
                if self.current == 0 || self.module_top() {
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
                let handler = if self.is("catch") && matches!(self.peek_at(1), Tok::Punct("{")) {
                    // `catch {`: the exception in a variable of its own.
                    self.advance();
                    let k = self.classes;
                    self.classes += 1;
                    let e: Name = format!("%catch{k}").into();
                    self.hidden_var(e.clone());
                    self.expect("{")?;
                    Some((e, self.scoped_block_rest()?))
                } else if self.eat("catch") {
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
                self.strict_word(&name)?;
                if self.labels.iter().any(|(f, l)| *f == self.current && *l == name) {
                    return self.err(format!("label `{name}` inside a statement of the same label"));
                }
                self.advance();
                self.labels.push((self.current, name.clone()));
                let body = self.sub_statement();
                self.labels.pop();
                StmtKind::Labeled(name, stmt(self.arena, body?))
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
        // (Not in strict code, nor a generator.)
        if self.strict() || self.peek_at(1) == &Tok::Punct("*") {
            return self.err("a declaration as a statement's body");
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
        let pos = self.pos();
        self.using_scopes.push(UsingScope { allowed: true, names: None, is_await: false });
        let body = self.statements_to_brace();
        let scope = self.using_scopes.pop().unwrap();
        let body = body?;
        Ok(match scope.names {
            Some(names) => self.lower_using(body, names, scope.is_await, pos),
            None => body,
        })
    }

    fn statements_to_brace(&mut self) -> R<Vec<Stmt<'a>>> {
        let mut body = Vec::new();
        while !self.eat("}") {
            if *self.peek() == Tok::Eof {
                return self.err("expected `}` before end of input");
            }
            body.push(self.statement()?);
        }
        // (The tree lives until the script is compiled: a vector's room
        // past its statements, 3 more for one, took Mandreel 40 MB.)
        body.shrink_to_fit();
        Ok(body)
    }

    /// Whether a `using` declaration starts here (`using x`, on one line),
    /// or with `await_`, an `await using` one (in an async function).
    fn using_ahead(&self, await_: bool) -> bool {
        let tok = |k: usize| &self.toks[(self.i + k).min(self.toks.len() - 1)];
        let ident = |k: usize, s: &str| matches!(self.peek_at(k), Tok::Ident(n) if &**n == s) && !tok(k).escaped;
        let binding = |k: usize| matches!(self.peek_at(k), Tok::Ident(_)) && !tok(k).newline_before;
        if await_ {
            self.functions[self.current].is_async
                && ident(0, "await")
                && ident(1, "using")
                && !tok(1).newline_before
                && binding(2)
        } else {
            ident(0, "using") && binding(1)
        }
    }

    /// Hidden variables for disposing of a statement list's resources (see
    /// [`Parser::lower_using`]).
    fn using_names(&mut self) -> UsingNames {
        let k = self.classes;
        self.classes += 1;
        let names = UsingNames {
            state: format!("%using{k}").into(),
            has: format!("%usinghas{k}").into(),
            error: format!("%usingerr{k}").into(),
            caught: format!("{}{k}", super::ast::USING_CATCH).into(),
            pending: format!("%usingres{k}").into(),
        };
        for n in [&names.state, &names.has, &names.error, &names.caught, &names.pending] {
            self.hidden_var(n.clone());
        }
        names
    }

    /// `using` declarations' bindings (`no_in`: in a `for` head, where
    /// they may lack an initializer): names only, as `const`.
    fn using_bindings(&mut self, no_in: bool) -> R<Vec<(Name, Option<Expr<'a>>)>> {
        let mut decls = Vec::new();
        loop {
            if self.is("[") || self.is("{") {
                return self.err("a `using` declaration binds names, not patterns");
            }
            let name = self.binding_ident()?;
            if &*name == "let" {
                return self.err("`let` can't be declared with `using`");
            }
            let init = if self.eat("=") {
                let e = self.assignment(no_in)?;
                self.infer_name(&e, &name);
                Some(e)
            } else {
                if !no_in {
                    return self.err("missing initializer in a `using` declaration");
                }
                None
            };
            let end = self.pos();
            self.check_not_param(&name)?;
            let r = self.declare(&name, CONST, end);
            decls.push((r, init));
            if !self.eat(",") {
                return Ok(decls);
            }
        }
    }

    /// `using` declarations' initializers, each value added to the
    /// resources of capability `state` as it is bound.
    fn using_inits(&self, decls: Vec<(Name, Option<Expr<'a>>)>, state: &Name, is_await: bool) -> Vec<(Name, Option<Expr<'a>>)> {
        decls
            .into_iter()
            .map(|(n, init)| {
                let init = init.map(|e| {
                    let pos = e.pos;
                    let args = vec![Expr { kind: ExprKind::Ident(state.clone()), pos }, e, Expr { kind: ExprKind::Bool(is_await), pos }];
                    Expr { kind: ExprKind::Intrinsic("dispose_add", args), pos }
                });
                (n, init)
            })
            .collect()
    }

    /// A `using` (`await using`, `is_await`) declaration statement, its
    /// resources the innermost statement list's.
    fn using_declaration(&mut self, is_await: bool) -> R<StmtKind<'a>> {
        if !self.using_scopes.last().is_some_and(|s| s.allowed) {
            return self.err("a `using` declaration isn't allowed here");
        }
        if is_await {
            self.advance();
            self.note_await();
        }
        self.advance();
        let decls = self.using_bindings(false)?;
        self.semicolon()?;
        let names = match self.using_scopes.last().unwrap().names.clone() {
            Some(n) => n,
            None => {
                let n = self.using_names();
                self.using_scopes.last_mut().unwrap().names = Some(n.clone());
                n
            }
        };
        if is_await {
            self.using_scopes.last_mut().unwrap().is_await = true;
        }
        Ok(StmtKind::Lexical(true, self.using_inits(decls, &names.state, is_await)))
    }

    /// Statement list `body` with `using` declarations, as
    /// `%state = new; %has = false; try { body } catch (%caught) { %has =
    /// true; %error = %caught; } finally { dispose }`: its resources
    /// disposed of however it ends, what disposing throws thrown instead
    /// (with `is_await`, awaited: if there are resources to dispose of).
    fn lower_using(&mut self, body: Vec<Stmt<'a>>, names: UsingNames, is_await: bool, pos: Pos) -> Vec<Stmt<'a>> {
        let id = |n: &Name| Expr { kind: ExprKind::Ident(n.clone()), pos };
        // (Not expression statements: the completion value isn't theirs.)
        let assign = |p: &Self, n: &Name, v: Expr<'a>| Stmt {
            kind: StmtKind::Lexical(
                false,
                vec![("".into(), Some(Expr { kind: ExprKind::Assign(None, expr(p.arena, id(n)), expr(p.arena, v)), pos }))],
            ),
            pos,
        };
        let state = Expr { kind: ExprKind::Intrinsic("dispose_new", Vec::new()), pos };
        let init = vec![assign(self, &names.state, state), assign(self, &names.has, Expr { kind: ExprKind::Bool(false), pos })];
        let catch = vec![
            assign(self, &names.has, Expr { kind: ExprKind::Bool(true), pos }),
            assign(self, &names.error, id(&names.caught)),
        ];
        let args = vec![id(&names.state), id(&names.has), id(&names.error)];
        let finally = if is_await {
            let run = Expr { kind: ExprKind::Intrinsic("dispose_run_async", args), pos };
            let wait = Stmt {
                kind: StmtKind::Lexical(false, vec![("".into(), Some(Expr { kind: ExprKind::Await(expr(self.arena, id(&names.pending))), pos }))]),
                pos,
            };
            vec![
                assign(self, &names.pending, run),
                Stmt { kind: StmtKind::If(id(&names.pending), stmt(self.arena, wait), None), pos },
            ]
        } else {
            vec![Stmt { kind: StmtKind::Lexical(false, vec![("".into(), Some(Expr { kind: ExprKind::Intrinsic("dispose_run", args), pos }))]), pos }]
        };
        let mut out = init;
        out.push(Stmt { kind: StmtKind::Try(body, Some((names.caught, catch)), Some(finally)), pos });
        out
    }

    /// `for (using x of it) body`, `for (using x = e; …; …) body` (or
    /// `await using`), past `(`: the first with each value disposed of
    /// after its iteration, the second with its resources after the loop.
    fn for_using(&mut self, is_await: bool) -> R<StmtKind<'a>> {
        let pos = self.pos();
        if is_await {
            self.advance();
            self.note_await();
        }
        self.advance();
        let decls = self.using_bindings(true)?;
        if self.is("in") {
            return self.err("a `using` declaration in a `for … in` head");
        }
        if decls.len() == 1 && decls[0].1.is_none() && self.is_of() {
            let name = decls.into_iter().next().unwrap().0;
            self.advance();
            let iterable = self.assignment(false)?;
            self.head_tdz(std::slice::from_ref(&name));
            self.expect(")")?;
            let body = self.sub_statement()?;
            let names = self.using_names();
            let args = vec![
                Expr { kind: ExprKind::Ident(names.state.clone()), pos },
                Expr { kind: ExprKind::Ident(name.clone()), pos },
                Expr { kind: ExprKind::Bool(is_await), pos },
            ];
            let first = Stmt {
                kind: StmtKind::Lexical(false, vec![("".into(), Some(Expr { kind: ExprKind::Intrinsic("dispose_add", args), pos }))]),
                pos,
            };
            let lowered = self.lower_using(vec![first, body], names, is_await, pos);
            let body = stmt(self.arena, Stmt { kind: StmtKind::Block(lowered), pos });
            return Ok(StmtKind::ForOf {
                kind: 3,
                target: expr(self.arena, Expr { kind: ExprKind::Ident(name), pos }),
                pattern: None,
                iterable: expr(self.arena, iterable),
                body,
                is_await: false,
            });
        }
        if decls.iter().any(|d| d.1.is_none()) {
            return self.err("missing initializer in a `using` declaration");
        }
        let names = self.using_names();
        let decls = self.using_inits(decls, &names.state, is_await);
        self.expect(";")?;
        let test = if self.is(";") { None } else { Some(self.expression(false)?) };
        self.expect(";")?;
        let update = if self.is(")") { None } else { Some(self.expression(false)?) };
        self.expect(")")?;
        let body = stmt(self.arena, self.sub_statement()?);
        let declaration = Stmt { kind: StmtKind::Lexical(true, decls), pos };
        let lp = Stmt {
            kind: StmtKind::For {
                init: None,
                test: test.map(|e| expr(self.arena, e)),
                update: update.map(|e| expr(self.arena, e)),
                body,
            },
            pos,
        };
        Ok(StmtKind::Block(self.lower_using(vec![declaration, lp], names, is_await, pos)))
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
            let written = self.binding_ident()?;
            self.check_var(&written)?;
            // (A body's `var` renamed apart: see `push_body_vars`.)
            let name = self.resolve_var(&written).unwrap_or_else(|| written.clone());
            let init = if self.eat("=") {
                let e = self.assignment(no_in)?;
                self.infer_name(&e, &written);
                Some(e)
            } else {
                None
            };
            // `catch (e) { var e = 1; }`: the `var` is the function's, the
            // initializer stores to the catch parameter (Annex B.3.5).
            if let (Some(target), Some(e)) = (self.resolve(&written).filter(|t| *t != name), &init) {
                let pos = e.pos;
                let target = expr(self.arena, Expr { kind: ExprKind::Ident(target), pos });
                let value = expr(self.arena, init.unwrap());
                decls.push((name, None));
                decls.push(("".into(), Some(Expr { kind: ExprKind::Assign(None, target, value), pos })));
            } else {
                decls.push((name, init));
            }
            if !self.eat(",") {
                return Ok(decls);
            }
        }
    }

    fn for_rest(&mut self) -> R<StmtKind<'a>> {
        self.expect("(")?;
        if self.using_ahead(true) {
            return self.for_using(true);
        }
        // (`for (using of …` is the identifier `using`, but `for (using of =
        // …;;)` declares `of`.)
        if self.using_ahead(false)
            && (!matches!(self.peek_at(1), Tok::Ident(n) if &**n == "of") || self.peek_at(2) == &Tok::Punct("="))
        {
            return self.for_using(false);
        }
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
                    object: expr(self.arena, object),
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
            Some(ForInit::Expr(expr(self.arena, e)))
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
            test: test.map(|e| expr(self.arena, e)),
            update: update.map(|e| expr(self.arena, e)),
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
            object: expr(self.arena, object),
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
            target: expr(self.arena, Expr { kind: target, pos }),
            pattern: None,
            iterable: expr(self.arena, iterable),
            body,
            is_await: false,
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
            is_await: false,
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
        for n in &names {
            match lexical {
                Some(_) => self.check_not_param(n)?,
                None => self.check_var(n)?,
            }
        }
        let names: Vec<Name> = match lexical {
            Some(kind) => {
                let map: Vec<(Name, Name)> = names.iter().map(|n| (n.clone(), self.declare(n, kind, end))).collect();
                rename_pattern(&mut pat, &map);
                map.into_iter().map(|(_, r)| r).collect()
            }
            // (A body's `var`s renamed apart, as `var_decls` does: see
            // `push_body_vars`. Not where a catch parameter is the binding
            // the store goes to.)
            None => {
                let map: Vec<(Name, Name)> = names
                    .iter()
                    .map(|n| {
                        let r = self.resolve_var(n).filter(|r| self.resolve(n).as_ref() == Some(r));
                        (n.clone(), r.unwrap_or_else(|| n.clone()))
                    })
                    .collect();
                if map.iter().any(|(n, r)| n != r) {
                    rename_pattern(&mut pat, &map);
                }
                map.into_iter().map(|(_, r)| r).collect()
            }
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
        self.deep()?;
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
                let d = self.pattern_default(&p)?;
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
                let d = self.pattern_default(&p)?;
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
    /// (An anonymous function's name that of a name pattern `p`.)
    fn pattern_default(&mut self, p: &Pattern) -> R<Option<&'a Expr<'a>>> {
        if !self.eat("=") {
            return Ok(None);
        }
        let e = self.assignment(false)?;
        if let Pattern::Name(n) = p {
            self.infer_name(&e, n);
        }
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
                let mut rest = None;
                for (i, (k, v)) in ps.iter().enumerate() {
                    if let ExprKind::Spread(t) = &v.kind {
                        if i + 1 < ps.len() || !matches!(t.kind, ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..)) {
                            return bad();
                        }
                        rest = Some(Box::new(self.to_pattern(t)?));
                        continue;
                    }
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
                Pattern::Object(props, rest)
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
            object: expr(self.arena, object),
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
            // (No `using` declarations directly in a clause.)
            self.using_scopes.push(UsingScope { allowed: false, names: None, is_await: false });
            while !self.is("case") && !self.is("default") && !self.is("}") {
                if *self.peek() == Tok::Eof {
                    self.using_scopes.pop();
                    return self.err("expected `}` before end of input");
                }
                match self.statement() {
                    Ok(s) => body.push(s),
                    Err(e) => {
                        self.using_scopes.pop();
                        return Err(e);
                    }
                }
            }
            self.using_scopes.pop();
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
            Tok::Ident(_) => {
                let outer = std::mem::replace(&mut self.naming_function, !declaration);
                let n = self.ident();
                self.naming_function = outer;
                Some(n?)
            }
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
            param_ends: Vec::new(),
            tdz_params: Vec::new(),
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
            fields: None,
            module: false,
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
        let mut bound = std::mem::take(&mut self.pattern_names);
        bound.extend(params.iter().cloned());
        // (No duplicates in non-simple parameters, or a method's.)
        let f = &self.functions[index];
        if (!prologue.is_empty() || f.rest || f.method || f.class_kind != 0) && has_duplicate(&bound) {
            return self.err("duplicate parameter names");
        }
        self.param_names.push((index, bound.clone()));
        let body_vars = if prologue.is_empty() { None } else { self.push_body_vars(index, &bound) };
        let body_pos = self.pos();
        self.expect("{")?;
        if self.directive_prologue() {
            // (Only with simple parameters.)
            if !prologue.is_empty() || self.functions[index].rest {
                return self.err("\"use strict\" in a function with non-simple parameters");
            }
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
        // (Nor in a parameter list: its references must be known, see
        // `Function::tdz_params`.)
        if self.skim
            && !paren
            && self.withs.is_empty()
            && prologue.is_empty()
            && !self.functions[index].rest
            && self.param_refs.is_empty()
        {
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
        let copies = body_vars.inspect(|_| {
            self.frames.pop();
        });
        let end = self.toks[self.i - 1].pos + 1;
        self.functions[index].end = end;
        let mut body = body?;
        self.functions[index].param_stmts = prologue.len();
        if let Some(copies) = copies {
            let copies = self.copy_params(copies, body_pos);
            body.splice(0..0, copies);
        }
        if !prologue.is_empty() {
            body.splice(0..0, prologue);
        }
        Ok((params, Some(body)))
    }

    /// After a function's `(`: its parameters, up to and including the
    /// `)`, and the statements giving them their values, each parameter's
    /// in turn: its default (`if (p === void 0) p = e;`), then its
    /// pattern's names. Sets the function's `length`, `rest`, `param_ends`
    /// and `tdz_params`; it's the current one.
    fn params(&mut self) -> R<(Vec<Name>, Vec<Stmt<'a>>)> {
        self.param_refs.push(Vec::new());
        let r = self.params_inner();
        let refs = self.param_refs.pop().unwrap_or_default();
        let (params, prologue, bound, ends) = r?;
        // The names a default or a pattern's may read before their
        // parameter's turn: referred to before its end (or after a direct
        // `eval`), by the defaults or the functions in them. (One such
        // function's own binding of a name holds the parameter all the
        // same, harmlessly: its references are to its own.)
        if refs.is_empty() {
            return Ok((params, prologue));
        }
        let eval = refs.iter().filter(|(n, _)| &**n == "eval").map(|&(_, p)| p).min();
        let mut tdz = Vec::new();
        for (n, j) in bound {
            if eval.is_some_and(|p| p < ends[j]) || refs.iter().any(|(r, p)| *r == n && *p < ends[j]) {
                tdz.push((n, j));
            }
        }
        if !tdz.is_empty() {
            let f = &mut self.functions[self.current];
            f.param_ends = ends;
            f.tdz_params = tdz;
        }
        Ok((params, prologue))
    }

    /// [`Parser::params`]'s parameters and statements, the names each
    /// parameter binds, and where each ends: from the first with a default
    /// or a pattern only (no reference comes before it), those before it
    /// ending at 0.
    #[allow(clippy::type_complexity)]
    fn params_inner(&mut self) -> R<(Vec<Name>, Vec<Stmt<'a>>, Vec<(Name, usize)>, Vec<Pos>)> {
        let mut params: Vec<Name> = Vec::new();
        let mut prologue = Vec::new();
        let mut length = None;
        let mut bound = Vec::new();
        let mut ends = Vec::new();
        let mut pattern_names = Vec::new();
        while !self.eat(")") {
            if !params.is_empty() {
                self.expect(",")?;
                if self.eat(")") {
                    break;
                }
            }
            let rest = self.eat("...");
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
            if rest {
                length.get_or_insert(params.len());
                self.functions[self.current].rest = true;
            } else if self.eat("=") {
                length.get_or_insert(params.len());
                let e = self.assignment(false)?;
                self.infer_name(&e, &p);
                prologue.push(self.default_stmt(p.clone(), e, pos));
            }
            let j = params.len();
            // (From the first parameter with a default or a pattern: see
            // above.)
            if !prologue.is_empty() || pattern.is_some() || !ends.is_empty() {
                ends.resize(j, 0);
                ends.push(self.pos());
            }
            let held = !ends.is_empty();
            match pattern {
                // Destructured: `var`s of the function, set after its
                // default (initialized, not stored: see `tdz_params`).
                Some((pat, names)) => {
                    bound.extend(names.iter().map(|n| (n.clone(), j)));
                    pattern_names.extend(names.iter().cloned());
                    let mut decls: Vec<(Name, Option<Expr<'a>>)> =
                        names.into_iter().map(|n| (n, Some(Expr { kind: ExprKind::Bound, pos }))).collect();
                    let value = expr(self.arena, Expr { kind: ExprKind::Ident(p.clone()), pos });
                    decls.push(("".into(), Some(Expr { kind: ExprKind::Destructure(Box::new(pat), value, 2), pos })));
                    prologue.push(Stmt { kind: StmtKind::Var(decls), pos });
                }
                None if held => bound.push((p.clone(), j)),
                None => {}
            }
            params.push(p);
            if rest {
                self.expect(")")?;
                break;
            }
        }
        self.pattern_names = pattern_names;
        self.functions[self.current].length = length.unwrap_or(params.len());
        Ok((params, prologue, bound, ends))
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
            param_ends: Vec::new(),
            tdz_params: Vec::new(),
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
            fields: None,
            module: false,
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
        // (An arrow's parameters are never duplicates.)
        let mut bound = std::mem::take(&mut self.pattern_names);
        bound.extend(params.iter().filter(|p| !p.starts_with('%')).cloned());
        if has_duplicate(&bound) {
            self.current = outer;
            return self.err("duplicate parameter names");
        }
        // (Its body's `var`s renamed apart from its defaults' closures, as a
        // function's: see `function_rest`.)
        let mut copies = None;
        let body_pos = self.pos();
        let body = if self.eat("{") {
            // (`"use strict"` of its own, with simple parameters only.)
            if self.directive_prologue() {
                if !prologue.is_empty() || self.functions[index].rest {
                    self.current = outer;
                    return self.err("\"use strict\" in a function with non-simple parameters");
                }
                self.functions[index].strict = true;
            }
            if !prologue.is_empty() {
                if self.collect {
                    self.table.defaults = true;
                }
                copies = self.push_body_vars(index, &bound);
            }
            let body = self.scoped_block_rest();
            if copies.is_some() {
                self.frames.pop();
            }
            body
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
        if let Some(copies) = copies {
            let copies = self.copy_params(copies, body_pos);
            body.splice(0..0, copies);
        }
        let f = &mut self.functions[index];
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
                Tok::Ident(n) if &**n == "let" || n.starts_with('#') => return None,
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
                        && matches!(next, Tok::Ident(_) | Tok::Str(_) | Tok::Num(_) | Tok::BigInt(_) | Tok::Keyword(_))
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
            // (A renamed `let`'s, `a%0`, is its source name's.)
            let name = match name.find('%') {
                Some(k) if k > 0 => name[..k].into(),
                _ => name.clone(),
            };
            self.functions[i].name = Some(name);
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
        self.deep()?;
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
        if let Tok::Punct(p @ ("&&=" | "||=" | "??=")) = self.peek() {
            let kind = match *p {
                "&&=" => 0,
                "||=" => 1,
                _ => 2,
            };
            if !matches!(target.kind, ExprKind::Ident(_) | ExprKind::Member(..) | ExprKind::Index(..)) {
                return self.err("invalid assignment target");
            }
            if let ExprKind::Ident(n) = &target.kind
                && self.strict()
                && matches!(&**n, "eval" | "arguments")
            {
                return self.err(format!("assignment to `{n}` in strict mode"));
            }
            let pos = self.pos();
            self.advance();
            let value = self.assignment(no_in)?;
            if let ExprKind::Ident(n) = &target.kind {
                let n = n.clone();
                self.infer_name(&value, &n);
            }
            return Ok(Expr {
                kind: ExprKind::LogicalAssign(kind, expr(self.arena, target), expr(self.arena, value)),
                pos,
            });
        }
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
                // (`o.n = function () {}`'s too, though the spec's is "":
                // its code's name, as profiles and listings show it.)
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
            // `#x in o`: whether `o` has the private name.
            if let (Bin::Op(BinOp::In), ExprKind::Ident(n)) = (&op, &l.kind)
                && n.starts_with('#')
            {
                l = Expr { kind: ExprKind::Intrinsic("private_has", vec![r, take(&mut l)]), pos };
                continue;
            }
            let kind = match op {
                Bin::Op(op) => ExprKind::Binary(op, expr(self.arena, l), expr(self.arena, r)),
                Bin::And => ExprKind::Logical(true, expr(self.arena, l), expr(self.arena, r)),
                Bin::Or => ExprKind::Logical(false, expr(self.arena, l), expr(self.arena, r)),
                Bin::Nullish => ExprKind::Nullish(expr(self.arena, l), expr(self.arena, r)),
            };
            l = Expr { kind, pos };
        }
    }

    fn unary(&mut self) -> R<Expr<'a>> {
        self.deep()?;
        let pos = self.pos();
        if self.functions[self.current].is_async
            && matches!(self.peek(), Tok::Ident(n) if &**n == "await")
            && !self.toks[self.i].escaped
        {
            self.advance();
            self.note_await();
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
        if op == UnOp::Delete
            && let ExprKind::Member(_, n) | ExprKind::Chain(Expr { kind: ExprKind::Member(_, n), .. }) = &e.kind
            && n.starts_with('#')
        {
            return self.err("deleting a private member");
        }
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
        // In an optional chain (`a?.b`): the whole of it, made `Chain`.
        let mut chain = false;
        loop {
            let pos = self.pos();
            if self.eat("?.") {
                chain = true;
                let base = expr(self.arena, Expr { kind: ExprKind::Optional(expr(self.arena, take(&mut e))), pos });
                let kind = if self.eat("(") {
                    ExprKind::Call(base, self.arguments()?)
                } else if self.eat("[") {
                    let k = self.expression(false)?;
                    self.expect("]")?;
                    ExprKind::Index(base, expr(self.arena, k))
                } else {
                    let name = self.property_name()?;
                    ExprKind::Member(base, self.private_ref(name)?)
                };
                e = Expr { kind, pos };
            } else if matches!(self.peek(), Tok::Template(t) if t.head) && chain {
                return self.err("a tagged template in an optional chain");
            } else if self.eat("(") {
                let args = self.arguments()?;
                let mut args = args;
                if matches!(&e.kind, ExprKind::Ident(n) if &**n == "eval") {
                    // A direct `eval` (see `Function::has_eval`), which may
                    // read `arguments`.
                    self.functions[self.current].has_eval = true;
                    let k = self.non_arrow();
                    self.functions[k].uses_arguments = true;
                    // (Its code may read `new.target`.)
                    if k != 0 {
                        self.functions[k].uses_new_target = true;
                    }
                    if self.functions[self.current].arrow {
                        self.functions[k].uses_this = true;
                    }
                    // The block-scoped bindings it sees, for its scope
                    // object (see `Gen::call`): a last argument the
                    // direct `eval`'s code takes apart. Then a function
                    // that refers to each of them (and to the enclosing
                    // functions' variables it sees: see
                    // `codegen::upvalues`), never called: its upvalues are
                    // theirs, which the scope object's accessors read and
                    // write.
                    // (A script's too where it sees any: its `let`s and
                    // the like, which aren't the global object's.)
                    if !args.iter().any(|a| matches!(a.kind, ExprKind::Spread(_))) {
                        let mut visible = self.eval_visible(pos);
                        if self.current != 0 || !visible.is_empty() {
                            let end = self.toks[self.i - 1].pos;
                            // (In an arrow, the function's `arguments` and
                            // `new.target` it sees too.)
                            let extra: &[&str] = if k != self.current && k != 0 { &["arguments", "%newtarget"] } else { &[] };
                            let f = self.eval_bindings(&visible, extra, end);
                            visible.push(Expr { kind: ExprKind::Function(f), pos });
                            args.push(Expr { kind: ExprKind::Intrinsic("%eval_scope", visible), pos });
                        }
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
                if chain {
                    let pos = e.pos;
                    e = Expr { kind: ExprKind::Chain(expr(self.arena, e)), pos };
                }
                return Ok(e);
            }
        }
    }

    /// `.name` or `[key]` applied to `e`, if one follows.
    fn member_suffix(&mut self, e: &mut Expr<'a>) -> R<bool> {
        let pos = self.pos();
        let kind = if self.eat(".") {
            let name = self.property_name()?;
            let name = self.private_ref(name)?;
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
        self.deep()?;
        let pos = self.pos();
        self.expect("new")?;
        if self.is(".") && matches!(self.peek_at(1), Tok::Ident(n) if &**n == "target") {
            self.advance();
            self.advance();
            let k = self.non_arrow();
            if k == 0 && !self.in_eval || self.functions[k].module {
                return self.err("`new.target` outside a function");
            }
            // (In direct `eval`'s code, its caller's: a binding of its
            // scope object, see `Gen::call`.)
            if k != 0 {
                self.functions[k].uses_new_target = true;
            }
            let mut e = Expr {
                kind: ExprKind::Ident("%newtarget".into()),
                pos,
            };
            while self.member_suffix(&mut e)? {}
            return Ok(e);
        }
        if self.is("import")
            && (self.peek_at(1) == &Tok::Punct("(")
                || self.peek_at(1) == &Tok::Punct(".") && matches!(self.peek_at(2), Tok::Ident(n) if &**n == "defer" || &**n == "source"))
        {
            return self.err("`new import(…)`");
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
        if self.is("?.") {
            return self.err("an optional chain in `new`");
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
        // (As a block's statements: see `statements_to_brace`.)
        args.shrink_to_fit();
        Ok(args)
    }

    /// An import call's arguments, past its `(`: the specifier, the
    /// options (`undefined` if none), and the referrer (this module, or
    /// the script: -1).
    fn import_call_args(&mut self, pos: Pos) -> R<Vec<Expr<'a>>> {
        if self.is(")") {
            return self.err("`import()` without a specifier");
        }
        let mut args = vec![self.assignment(false)?];
        if self.eat(",") && !self.is(")") {
            args.push(self.assignment(false)?);
            self.eat(",");
        }
        self.expect(")")?;
        if args.len() < 2 {
            let zero = expr(self.arena, Expr { kind: ExprKind::Num(0.0), pos });
            args.push(Expr { kind: ExprKind::Unary(UnOp::Void, zero), pos });
        }
        args.push(Expr { kind: ExprKind::Num(self.module.unwrap_or(-1) as f64), pos });
        Ok(args)
    }

    fn primary(&mut self) -> R<Expr<'a>> {
        let pos = self.pos();
        let start = self.i;
        let kind = match self.bump() {
            Tok::Num(v) => ExprKind::Num(v),
            Tok::BigInt(d) => ExprKind::Intrinsic("bigint", vec![Expr { kind: ExprKind::Str(d.into()), pos }]),
            Tok::Str(s) => ExprKind::Str(s.into()),
            // `import.defer(specifier, options)`, `import.source(…)`: as
            // `import()`, a deferred namespace or a module source.
            Tok::Keyword("import") if self.is(".") && (matches!(self.peek_at(1), Tok::Ident(n) if &**n == "defer" || &**n == "source")) => {
                self.advance();
                let phase = if self.is_word("defer") { 1.0 } else if self.is_word("source") { 2.0 } else {
                    return self.err("an escaped `defer` or `source` after `import.`");
                };
                self.advance();
                self.expect("(")?;
                let mut args = self.import_call_args(pos)?;
                args.push(Expr { kind: ExprKind::Num(phase), pos });
                ExprKind::Intrinsic("dynamic_import", args)
            }
            // `import.meta`, a module's (see `module::import_meta`).
            Tok::Keyword("import") if self.eat(".") => {
                if !self.is_word("meta") {
                    return self.err("expected `meta` after `import.`");
                }
                self.advance();
                let Some(id) = self.module else {
                    return self.err("`import.meta` outside a module");
                };
                ExprKind::Intrinsic("import_meta", vec![Expr { kind: ExprKind::Num(id as f64), pos }])
            }
            // `import(specifier, options)`: a promise of the module's
            // namespace (see `module::dynamic_import`), from this module's
            // (or the script's: -1).
            Tok::Keyword("import") if self.eat("(") => {
                let mut args = self.import_call_args(pos)?;
                args.push(Expr { kind: ExprKind::Num(0.0), pos });
                ExprKind::Intrinsic("dynamic_import", args)
            }
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
            // `#x in o` (see `binary`).
            Tok::Ident(s) if s.starts_with('#') => {
                if !self.is("in") {
                    self.i = start;
                    return self.unexpected();
                }
                ExprKind::Ident(self.private_ref(s)?)
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
                ExprKind::Template(strings.into(), subs)
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
                    // `...e`: its own enumerable properties copied.
                    if self.eat("...") {
                        let v = self.assignment(false)?;
                        let v = Expr { kind: ExprKind::Spread(expr(self.arena, v)), pos: ppos };
                        props.push((PropKey::Name("...".into()), v));
                        if !self.is("}") {
                            self.expect(",")?;
                        }
                        continue;
                    }
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
                            self.strict_word(&n)?;
                            let kind = self.reference(n.clone());
                            let mut v = Expr { kind, pos: ppos };
                            // `{a = 1}`: only as a pattern (see `to_pattern`).
                            if self.eat("=") {
                                let d = self.assignment(false)?;
                                self.infer_name(&d, &n);
                                v = Expr {
                                    kind: ExprKind::Assign(None, expr(self.arena, v), expr(self.arena, d)),
                                    pos: ppos,
                                };
                            }
                            props.push((proto_own(PropKey::Name(n), ppos), v));
                        } else {
                            let key = self.prop_key()?;
                            if self.is("(") {
                                let h = self.object_home(&mut home);
                                self.pending_home = Some((Some(h), None, 0));
                                let f = self.method(&key, None, ppos)?;
                                props.push((proto_own(key, ppos), Expr { kind: ExprKind::Function(f), pos: ppos }));
                            } else {
                                self.expect(":")?;
                                let v = self.assignment(false)?;
                                // (`__proto__: v` sets the prototype: no name.)
                                if let PropKey::Name(k) = &key
                                    && &**k != "__proto__"
                                {
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
            Tok::RegExp(r) => {
                // (An invalid pattern or flags are an early error.)
                if let Err(m) = crate::regexp::Regex::check(&r.0, &r.1) {
                    return Err(Error::new(pos as usize, m));
                }
                ExprKind::RegExp(r)
            }
            _ => {
                self.i = start;
                return self.unexpected();
            }
        };
        Ok(Expr { kind, pos })
    }
}

/// Whether `s` is a reserved word (never an identifier, even escaped).
fn is_reserved_word(s: &str) -> bool {
    matches!(
        s,
        "break" | "case" | "catch" | "class" | "const" | "continue" | "debugger" | "default" | "delete" | "do"
            | "else" | "enum" | "export" | "extends" | "false" | "finally" | "for" | "function" | "if" | "import"
            | "in" | "instanceof" | "new" | "null" | "return" | "super" | "switch" | "this" | "throw" | "true"
            | "try" | "typeof" | "var" | "void" | "while" | "with"
    )
}

/// Whether `names` has one twice.
fn has_duplicate(names: &[Name]) -> bool {
    names.iter().enumerate().any(|(k, n)| names[..k].contains(n))
}

/// A shorthand's or a method's key `__proto__` as a computed one: an own
/// property (only `__proto__: v` sets the prototype).
fn proto_own(key: PropKey<'_>, pos: Pos) -> PropKey<'_> {
    match key {
        PropKey::Name(n) if &*n == "__proto__" => PropKey::Computed(Expr { kind: ExprKind::Str(n.to_string()), pos }),
        k => k,
    }
}

impl<'a> Parser<'_, 'a, '_> {
    /// A reference to the variable `name`: to the block-scoped binding it
    /// names, if any, by that binding's new name (each is renamed apart:
    /// `x%3`, a function's variable as any other, which the code
    /// generator knows from [`Program::lexicals`]).
    fn reference(&mut self, name: Name) -> ExprKind<'a> {
        if !self.param_refs.is_empty() {
            let pos = self.toks[self.i.saturating_sub(1)].pos;
            for refs in &mut self.param_refs {
                refs.push((name.clone(), pos));
            }
        }
        if &*name == "arguments" {
            // (An arrow's are the enclosing function's; a module has
            // none: a global's.)
            let k = self.non_arrow();
            if !self.functions[k].module {
                self.functions[k].uses_arguments = true;
            }
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

    /// What `var name` declares: [`Parser::resolve`]'s, but past catch
    /// parameters, which a `var` of the same name doesn't redeclare.
    fn resolve_var(&self, name: &Name) -> Option<Name> {
        if self.collect || self.renames == 0 {
            return None;
        }
        for f in self.frames.iter().rev().filter(|f| !f.catch) {
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
    /// in its body (its first pass's), renamed apart (its frame pushed), if
    /// any: a parameter's name too where the body declares it again, the
    /// `var`s of those to copy the parameter's value to, `(parameter, var)`.
    fn push_body_vars(&mut self, index: usize, params: &[Name]) -> Option<Vec<(Name, Name)>> {
        let prev = self.prev?;
        let vars = self.frames.iter().rev().find(|f| f.vars)?;
        let pos = self.toks[vars.key].pos;
        let fs = &prev.functions;
        let k = fs[1..].partition_point(|f| f.params_pos < pos) + 1;
        let f = fs.get(k).filter(|f| f.params_pos == pos)?;
        let own = f.name.clone().filter(|_| f.binds_name);
        // (What the body itself declares, past its parameters' statements,
        // which declare their patterns' names: its `var`s, and the
        // functions at its top level, not in a block, whose names aren't
        // the body's when a parameter's: Annex B.3.2.1.)
        let (mut vars, mut functions) = (Vec::new(), Vec::new());
        for s in &f.body[f.param_stmts.min(f.body.len())..] {
            declared_in(s, fs, &mut vars, false);
            if let StmtKind::Function(i) = &s.kind {
                functions.extend(fs[*i].name.clone());
            }
        }
        let mut names = Vec::new();
        let mut copies = Vec::new();
        for n in var_names(fs, k) {
            if &*n == "arguments" || Some(&n) == own.as_ref() || names.iter().any(|(o, _)| *o == n) {
                continue;
            }
            // A parameter's name the body declares again is a binding of
            // the body's, its value the parameter's where the body starts
            // (a function of the body's its own).
            let param = params.contains(&n);
            if param && !vars.contains(&n) && !functions.contains(&n) {
                continue;
            }
            let r: Name = format!("{n}%{}", self.renames).into();
            self.renames += 1;
            self.body_vars.insert(r.clone(), n.clone());
            if param {
                self.shadowed_params.insert((index, n.clone()));
                if !functions.contains(&n) {
                    copies.push((n.clone(), r.clone()));
                }
            }
            names.push((n, r));
        }
        if names.is_empty() {
            return None;
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
        Some(copies)
    }

    /// The statements copying parameters' values to the body's `var`s of
    /// their names (see `push_body_vars`), at the body's start.
    fn copy_params(&self, copies: Vec<(Name, Name)>, pos: Pos) -> Vec<Stmt<'a>> {
        let x = |k| Expr { kind: k, pos };
        copies
            .into_iter()
            .map(|(p, v)| {
                let target = expr(self.arena, x(ExprKind::Ident(v)));
                let value = expr(self.arena, x(ExprKind::Ident(p)));
                Stmt { kind: StmtKind::Expr(x(ExprKind::Assign(None, target, value))), pos }
            })
            .collect()
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
            // (Sloppy code's block functions may share a name, Annex B.)
            let strict = self.functions[self.current].strict;
            let decls = self.table.decls.entry(key).or_default();
            if decls.iter().any(|&(ref n, k)| {
                n == name && !(k == BLOCK_FUNCTION && kind == BLOCK_FUNCTION && !strict) && k != CATCH && kind != CATCH
            }) && self.redeclared.is_none()
            {
                self.redeclared = Some((self.toks[self.i.saturating_sub(1)].pos, format!("`{name}` has already been declared")));
            }
            decls.push((name.clone(), kind));
            return name.clone();
        }
        let Some((_, r)) = frame.names.iter().find(|(o, _)| o == name) else {
            return name.clone();
        };
        let r = r.clone();
        let switch = frame.switch;
        let frame_key = frame.key;
        self.lexicals.insert(
            r.clone(),
            Lexical {
                original: name.clone(),
                constant: kind == CONST || kind == IMPORT,
                hoisted: kind == BLOCK_FUNCTION || kind == CATCH || kind == IMPORT,
                end,
                func: self.current,
                switch,
                withs: self.withs.len(),
                module_top: frame_key == MODULE_SCOPE,
            },
        );
        r
    }

    /// A `var` of `name` here: an error if a block-scoped declaration of
    /// it (not a catch parameter's) is in a scope between here and the
    /// function's.
    fn check_var(&self, name: &Name) -> R<()> {
        if self.collect {
            return Ok(());
        }
        for frame in self.frames.iter().rev() {
            if frame.vars {
                break;
            }
            if frame.catch {
                continue;
            }
            // (A function declared directly in a body is a `var` too.)
            let clash = self.table.decls.get(&frame.key).is_some_and(|ds| {
                ds.iter().any(|(n, k)| n == name && *k != CATCH && !(*k == BLOCK_FUNCTION && frame.body))
            });
            if clash {
                return self.err(format!("`{name}` has already been declared"));
            }
        }
        Ok(())
    }

    /// A block-scoped declaration of `name` here: an error directly in a
    /// function's body if a parameter has the name.
    fn check_not_param(&self, name: &Name) -> R<()> {
        let params = self.param_names.iter().rev().find(|(f, _)| *f == self.current);
        if self.frames.last().is_some_and(|fr| fr.body)
            && self.current != 0
            && params.is_some_and(|(_, ns)| ns.contains(name))
        {
            return self.err(format!("`{name}` has already been declared"));
        }
        Ok(())
    }

    /// The block-scoped bindings (and catch parameters) a direct `eval`
    /// here sees, innermost first, as triples: the name, the binding (a
    /// variable), and its kind (`EVAL_CONST`…). (Not one a function's `var`
    /// between here and it shadows; the functions' own variables are the
    /// code generator's to add.)
    fn eval_visible(&self, pos: Pos) -> Vec<Expr<'a>> {
        let mut out = Vec::new();
        let mut seen: Vec<Name> = Vec::new();
        // (Past the function's own variables: enclosing functions'.)
        let mut outer = false;
        for frame in self.frames.iter().rev() {
            if frame.vars {
                seen.extend(frame.names.iter().map(|(o, _)| o.clone()));
                outer = true;
                continue;
            }
            for (orig, renamed) in &frame.names {
                if orig == renamed || orig.contains('%') || seen.contains(orig) {
                    continue;
                }
                seen.push(orig.clone());
                let constant = self.table.decls.get(&frame.key).is_some_and(|ds| ds.iter().any(|(n, k)| n == orig && (*k == CONST || *k == IMPORT)));
                let kind = (if frame.catch { EVAL_CATCH } else if constant { EVAL_CONST } else { 0 }) | if outer { EVAL_OUTER } else { 0 };
                out.push(Expr { kind: ExprKind::Str(orig.to_string()), pos });
                out.push(Expr { kind: ExprKind::Ident(renamed.clone()), pos });
                out.push(Expr { kind: ExprKind::Num(kind as f64), pos });
            }
        }
        out
    }

    /// The function a direct `eval` at `pos` (its call's end) makes for
    /// the bindings it sees (`visible`, of [`Parser::eval_visible`]): an
    /// arrow named [`EVAL_BINDINGS`] whose body refers to each.
    fn eval_bindings(&mut self, visible: &[Expr<'a>], extra: &[&str], pos: Pos) -> usize {
        let refer = |v: Name| Stmt { kind: StmtKind::Expr(Expr { kind: ExprKind::Ident(v), pos }), pos };
        let body = visible
            .chunks(3)
            .filter_map(|t| match &t[1].kind {
                ExprKind::Ident(v) => Some(refer(v.clone())),
                _ => None,
            })
            .chain(extra.iter().map(|&n| refer(n.into())))
            .collect();
        let f = self.synthetic_method(&Name::from(""), pos);
        let strict = self.functions[self.current].strict;
        let func = &mut self.functions[f];
        (func.name, func.body, func.arrow, func.method, func.home, func.strict) =
            (Some(Name::from(EVAL_BINDINGS)), body, true, false, None, strict);
        f
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
            let name = self.binding_ident()?;
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
            self.check_not_param(&name)?;
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
            Tok::BigInt(d) => crate::bigint::literal_key(&d).into(),
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
        // (A method's parameters are never duplicates.)
        if self.param_names.iter().rev().find(|(g, _)| *g == f).is_some_and(|(_, ns)| has_duplicate(ns)) {
            return self.err("duplicate parameter names");
        }
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
        if !self.is("{") {
            return self.err("expected `{`");
        }
        // Its private names, wherever in its body declared: variables.
        let privates: Vec<(Name, Name)> =
            self.private_names().into_iter().map(|n| (format!("{n}%p{k}").into(), n)).collect();
        for (v, _) in &privates {
            self.hidden_var(v.clone());
        }
        self.privates.push(privates.iter().map(|(v, n)| (n.clone(), v.clone())).collect());
        self.advance();
        let mut constructor = None;
        let mut members = Vec::new();
        // The function defining the instance fields, and its statements:
        // the private methods', then the fields'.
        let mut fields_fn = None;
        let mut method_stmts = Vec::new();
        let mut field_stmts = Vec::new();
        let mut statics = Vec::new();
        // The private names declared so far: kind (0 a field or method, 1
        // a getter, 2 a setter) and whether static.
        let mut declared: Vec<(Name, u8, bool)> = Vec::new();
        while !self.eat("}") {
            if self.eat(";") {
                continue;
            }
            // `static { … }`
            if matches!(self.peek(), Tok::Ident(n) if &**n == "static") && self.peek_at(1) == &Tok::Punct("{") {
                let f = self.static_block(&home_static)?;
                statics.push(f);
                continue;
            }
            let is_static = matches!(self.peek(), Tok::Ident(n) if &**n == "static")
                && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}"));
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
                Tok::Ident(n)
                    if matches!(&**n, "get" | "set")
                        && !matches!(self.peek_at(1), Tok::Punct("(" | "=" | ";" | "}"))
                        && !self.toks[self.i + 1].newline_before =>
                {
                    let k = if &**n == "get" { 1 } else { 2 };
                    self.advance();
                    k
                }
                _ => 0,
            };
            let key = self.prop_key()?;
            let private = match &key {
                PropKey::Name(n) if n.starts_with('#') => {
                    if &**n == "#constructor" {
                        return self.err("a private name `#constructor`");
                    }
                    // (A getter and a setter may share one.)
                    let clash = declared.iter().any(|(d, k, st)| {
                        d == n && !(*st == is_static && (*k, kind) == (1, 2) || *st == is_static && (*k, kind) == (2, 1))
                    });
                    if clash || declared.iter().filter(|(d, ..)| d == n).count() > 1 {
                        return self.err(format!("private name `{n}` declared twice"));
                    }
                    declared.push((n.clone(), kind, is_static));
                    Some(self.private_ref(n.clone())?)
                }
                _ => None,
            };
            // A field: no `(` after its key.
            if kind == 0 && !generator && !is_async && !self.is("(") {
                if let PropKey::Name(n) = &key
                    && (&**n == "constructor" || is_static && &**n == "prototype")
                {
                    return self.err(format!("a field named `{n}`"));
                }
                let fpos = self.pos();
                let h = if is_static { &home_static } else { &home };
                let f = match fields_fn {
                    Some(f) if !is_static => f,
                    _ => {
                        let f = self.synthetic_method(h, fpos);
                        if !is_static {
                            fields_fn = Some(f);
                        }
                        f
                    }
                };
                let x = |kind| Expr { kind, pos: fpos };
                let (which, key) = match (&private, key) {
                    (Some(v), _) => ("private_define", x(ExprKind::Ident(v.clone()))),
                    (None, PropKey::Name(n)) => ("define_field", x(ExprKind::Str(n.to_string()))),
                    (None, PropKey::Computed(e)) => {
                        // (Evaluated with the class, in order.)
                        let v: Name = format!("%key{k}_{}", members.len()).into();
                        self.hidden_var(v.clone());
                        members.push(ClassMember {
                            key: PropKey::Computed(e),
                            kind: 3,
                            is_static,
                            function: usize::MAX,
                            var: Some(v.clone()),
                        });
                        ("define_field", x(ExprKind::Ident(v)))
                    }
                };
                let value = if self.eat("=") {
                    let outer = std::mem::replace(&mut self.current, f);
                    let v = self.assignment(false);
                    self.current = outer;
                    let v = v?;
                    match &key.kind {
                        ExprKind::Str(n) => self.infer_name(&v, &n.as_str().into()),
                        ExprKind::Ident(_) if private.is_some() => {
                            let n = privates.iter().find(|(pv, _)| Some(pv) == private.as_ref()).unwrap().1.clone();
                            self.infer_name(&v, &n);
                        }
                        _ => {}
                    }
                    v
                } else {
                    let zero = expr(self.arena, x(ExprKind::Num(0.0)));
                    x(ExprKind::Unary(UnOp::Void, zero))
                };
                if !self.eat(";") && !self.is("}") && !self.toks[self.i].newline_before {
                    return self.err("expected `;` after a field");
                }
                let stmt = Stmt {
                    kind: StmtKind::Expr(x(ExprKind::Intrinsic(which, vec![x(ExprKind::This), key, value]))),
                    pos: fpos,
                };
                if is_static {
                    self.functions[f].body = vec![stmt];
                    self.functions[f].end = self.toks[self.i - 1].pos + 1;
                    statics.push(f);
                } else {
                    field_stmts.push(stmt);
                }
                continue;
            }
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
            // A private method: its function in a variable, defined on each
            // instance (on the class, a static one).
            let var = match &private {
                Some(v) => {
                    let fv: Name = format!("{v}${kind}").into();
                    self.hidden_var(fv.clone());
                    if !is_static {
                        let x = |kind| Expr { kind, pos: mpos };
                        let args = vec![
                            x(ExprKind::This),
                            x(ExprKind::Ident(v.clone())),
                            x(ExprKind::Ident(fv.clone())),
                            x(ExprKind::Num(kind as f64)),
                        ];
                        method_stmts.push(Stmt {
                            kind: StmtKind::Expr(x(ExprKind::Intrinsic("private_method", args))),
                            pos: mpos,
                        });
                    }
                    Some(fv)
                }
                None => None,
            };
            members.push(ClassMember {
                key,
                kind,
                is_static,
                function: f,
                var,
            });
        }
        self.privates.pop();
        let end = self.toks[self.i - 1].pos;
        let fields = if fields_fn.is_some() || !method_stmts.is_empty() {
            let f = match fields_fn {
                Some(f) => f,
                None => self.synthetic_method(&home, end),
            };
            method_stmts.append(&mut field_stmts);
            self.functions[f].body = method_stmts;
            self.functions[f].end = end + 1;
            let v: Name = format!("%fields{k}").into();
            self.hidden_var(v.clone());
            Some((v, f))
        } else {
            None
        };
        let constructor = match constructor {
            Some(f) => f,
            None => self.default_constructor(name.clone(), class_kind, &home, &home_static, end),
        };
        // (Its text is the class's, from `class`.)
        self.functions[constructor].text = Some((pos, end + 1));
        if let Some((v, _)) = &fields {
            self.functions[constructor].fields = Some(v.clone());
            for (c, f) in std::mem::take(&mut self.super_fns) {
                if c == home_static {
                    self.functions[f].fields = Some(v.clone());
                } else {
                    self.super_fns.push((c, f));
                }
            }
        }
        let inner = match &name {
            Some(n) => {
                let r = self.declare(n, CONST, end + 1);
                // (A variable of the function: not, in the script, a
                // global.)
                if r != *n {
                    self.functions[self.current].hidden.push(r.clone());
                }
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
            privates,
            fields,
            statics,
        })
    }

    /// The private names (`#x`) the class body at `self.i` (its `{`)
    /// declares: those directly in it not after `.` or before `in`.
    fn private_names(&self) -> Vec<Name> {
        let mut out: Vec<Name> = Vec::new();
        let mut depth = 0;
        let mut j = self.i;
        loop {
            match &self.toks[j].tok {
                Tok::Punct("{") => depth += 1,
                Tok::Punct("}") => {
                    depth -= 1;
                    if depth == 0 {
                        return out;
                    }
                }
                Tok::Eof => return out,
                Tok::Ident(n)
                    if depth == 1
                        && n.starts_with('#')
                        && !matches!(self.toks[j - 1].tok, Tok::Punct("." | "?."))
                        && self.toks[j + 1].tok != Tok::Keyword("in")
                        && !out.contains(n) =>
                {
                    out.push(n.clone())
                }
                _ => {}
            }
            j += 1;
        }
    }

    /// Property name `name`, or if a private name, its variable (in the
    /// innermost class declaring it).
    fn private_ref(&self, name: Name) -> R<Name> {
        if !name.starts_with('#') {
            return Ok(name);
        }
        for scope in self.privates.iter().rev() {
            if let Some((_, v)) = scope.iter().find(|(n, _)| *n == name) {
                return Ok(v.clone());
            }
        }
        self.err(format!("private name `{name}` not declared in an enclosing class"))
    }

    /// A function the parser makes for a class (its fields' initializers,
    /// a static block's code), from `pos`: a method of home object `home`,
    /// never compiled lazily.
    fn synthetic_method(&mut self, home: &Name, pos: Pos) -> usize {
        let index = self.functions.len();
        self.functions.push(Function {
            name: None,
            params: Vec::new(),
            body: Vec::new(),
            parent: Some(self.current),
            declaration: false,
            binds_name: false,
            uses_arguments: false,
            pos,
            params_pos: pos,
            end: pos,
            eager: true,
            strict: true,
            withs: self.withs.clone(),
            arrow: false,
            uses_this: false,
            method: true,
            length: 0,
            rest: false,
            param_stmts: 0,
            param_ends: Vec::new(),
            tdz_params: Vec::new(),
            binding: None,
            annex_b: false,
            class_kind: 0,
            has_eval: false,
            generator: false,
            is_async: false,
            uses_new_target: false,
            hidden: Vec::new(),
            text: None,
            home: Some(home.clone()),
            ctor_var: None,
            free: None,
            fields: None,
            module: false,
        });
        index
    }

    // ---- Modules ----

    /// A module's body, as the body of a function in the script (see
    /// `ast::MODULE_FUNCTION`): strict, a generator the loader calls to
    /// make the module's bindings (its functions, its other variables
    /// uninitialized) and resumes to run it, async if it has a top-level
    /// `await` (see `module`). Its parameters: the scope object, then the
    /// holders of its imports (see `ast::Import`). The script's statement:
    /// an expression of the function.
    fn module_body(&mut self) -> R<Stmt<'a>> {
        let pos = self.pos();
        let index = self.functions.len();
        self.functions.push(Function {
            name: Some(MODULE_FUNCTION.into()),
            params: Vec::new(),
            body: Vec::new(),
            parent: Some(0),
            declaration: false,
            binds_name: false,
            uses_arguments: false,
            pos,
            params_pos: pos,
            end: 0,
            eager: true,
            strict: true,
            withs: Vec::new(),
            arrow: false,
            uses_this: false,
            method: false,
            length: 0,
            rest: false,
            param_stmts: 0,
            param_ends: Vec::new(),
            tdz_params: Vec::new(),
            binding: None,
            annex_b: false,
            class_kind: 0,
            has_eval: false,
            // (A generator once parsed: `yield` is no expression in it.
            // Async while parsed: `await` is one at its top level.)
            generator: false,
            is_async: true,
            uses_new_target: false,
            hidden: Vec::new(),
            text: None,
            home: None,
            ctor_var: None,
            free: None,
            fields: None,
            module: true,
        });
        self.current = index;
        self.push_vars(self.i);
        self.push_scope(MODULE_SCOPE, false, true);
        self.using_scopes.push(UsingScope { allowed: true, names: None, is_await: false });
        let r = (|| {
            let mut body = Vec::new();
            while self.peek() != &Tok::Eof {
                body.push(self.module_item()?);
            }
            Ok(body)
        })();
        let scope = self.using_scopes.pop().unwrap();
        let r = r.and_then(|body| {
            self.functions[index].body = body;
            self.check_module(index)
        });
        self.frames.pop();
        self.frames.pop();
        self.current = 0;
        r?;
        let mut body = std::mem::take(&mut self.functions[index].body);
        if let Some(names) = scope.names {
            // (Its functions out of the `try`: made before it runs.)
            let (mut fns, rest): (Vec<_>, Vec<_>) = body.into_iter().partition(|s| matches!(s.kind, StmtKind::Function(_)));
            fns.extend(self.lower_using(rest, names, scope.is_await, pos));
            body = fns;
        }
        let imports = self.info.imports.len();
        let f = &mut self.functions[index];
        f.body = body;
        f.end = self.toks[self.i].pos;
        f.params = std::iter::once(MODULE_ENV.into()).chain((0..imports).map(|k| format!("%imp{k}").into())).collect();
        f.generator = true;
        f.is_async = self.info.has_await;
        Ok(Stmt { kind: StmtKind::Expr(Expr { kind: ExprKind::Function(index), pos }), pos })
    }

    /// Whether the code being parsed is a module's own (not a function's
    /// in it).
    fn module_top(&self) -> bool {
        self.functions[self.current].module
    }

    /// An `await` at a module's top level: the module is async.
    fn note_await(&mut self) {
        if self.module_top() {
            self.info.has_await = true;
        }
    }

    /// A statement of a module's body: an import or export declaration, or
    /// any other.
    fn module_item(&mut self) -> R<Stmt<'a>> {
        match self.peek() {
            Tok::Keyword("import") if !matches!(self.peek_at(1), Tok::Punct("(" | ".")) => self.import_declaration(),
            Tok::Keyword("export") => self.export_declaration(),
            _ => self.statement(),
        }
    }

    /// Whether the current token is contextual keyword `word` (as written:
    /// not escaped).
    fn is_word(&self, word: &str) -> bool {
        matches!(self.peek(), Tok::Ident(n) if &**n == word) && !self.toks[self.i].escaped
    }

    fn expect_word(&mut self, word: &str) -> R<()> {
        if !self.is_word(word) {
            return self.err(format!("expected `{word}`"));
        }
        self.advance();
        Ok(())
    }

    /// A ModuleExportName: an identifier name (a reserved word too) or a
    /// string (well-formed Unicode).
    fn module_export_name(&mut self) -> R<ExportName> {
        match self.bump() {
            Tok::Ident(n) => Ok(ExportName::Ident(n)),
            Tok::Keyword(k) => Ok(ExportName::Reserved(k.into())),
            Tok::Str(s) => {
                if super::lex::has_lone_surrogate(&s) {
                    self.i -= 1;
                    return self.err("an export name that isn't well-formed Unicode");
                }
                Ok(ExportName::Str(s.into()))
            }
            _ => {
                self.i -= 1;
                self.err("expected a name")
            }
        }
    }

    /// A module specifier and its `with { … }` attributes: the module's
    /// request (see `ModuleInfo::requests`).
    fn module_request(&mut self) -> R<usize> {
        self.module_request_in(Phase::Evaluation)
    }

    /// [`Parser::module_request`] of phase `phase`.
    fn module_request_in(&mut self, phase: Phase) -> R<usize> {
        if !matches!(self.peek(), Tok::Str(_)) {
            return self.err("expected a module specifier");
        }
        let Tok::Str(spec) = self.bump() else { unreachable!() };
        let mut ty = None;
        if self.eat("with") {
            self.expect("{")?;
            let mut keys: Vec<String> = Vec::new();
            while !self.eat("}") {
                let key = match self.bump() {
                    Tok::Ident(n) => n.to_string(),
                    Tok::Keyword(k) => k.to_string(),
                    Tok::Str(s) => s.into(),
                    _ => {
                        self.i -= 1;
                        return self.err("expected an attribute's key");
                    }
                };
                self.expect(":")?;
                let Tok::Str(value) = self.bump() else {
                    self.i -= 1;
                    return self.err("expected an attribute's value, a string");
                };
                if keys.contains(&key) {
                    return self.err(format!("duplicate attribute `{key}`"));
                }
                if key == "type" {
                    ty = Some(value.into());
                }
                keys.push(key);
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
        }
        let spec = String::from_utf16_lossy(&super::lex::to_units(&spec));
        let request = (spec, ty, phase);
        Ok(match self.info.requests.iter().position(|r| *r == request) {
            Some(k) => k,
            None => {
                self.info.requests.push(request);
                self.info.requests.len() - 1
            }
        })
    }

    /// `import …;`, past nothing: its bindings declared (see
    /// [`Parser::import_binding`]); no statement of its own.
    fn import_declaration(&mut self) -> R<Stmt<'a>> {
        let pos = self.pos();
        self.advance();
        if matches!(self.peek(), Tok::Str(_)) {
            self.module_request()?;
            self.semicolon()?;
            return Ok(Stmt { kind: StmtKind::Empty, pos });
        }
        // `import defer * as ns from "m"`, `import source x from "m"` (a
        // binding named `defer` or `source` otherwise).
        let word_at = |p: &Self, k: usize, w: &str| matches!(p.peek_at(k), Tok::Ident(n) if &**n == w) && !p.toks[p.i + k].escaped;
        let ident_at = |p: &Self, k: usize| matches!(p.peek_at(k), Tok::Ident(_) | Tok::Keyword("yield" | "await" | "let" | "static"));
        let phase = if self.is_word("defer") && self.peek_at(1) == &Tok::Punct("*") {
            self.advance();
            Phase::Defer
        } else if self.is_word("source") && ident_at(self, 1) && (!word_at(self, 1, "from") || word_at(self, 2, "from")) {
            self.advance();
            Phase::Source
        } else {
            Phase::Evaluation
        };
        if phase == Phase::Source {
            let local = self.binding_ident()?;
            self.expect_word("from")?;
            let request = self.module_request_in(phase)?;
            self.semicolon()?;
            self.import_binding(request, ImportName::Source, local);
            return Ok(Stmt { kind: StmtKind::Empty, pos });
        }
        // (What is imported; the binding.)
        let mut bindings: Vec<(ImportName, Name)> = Vec::new();
        let mut more = true;
        if phase == Phase::Evaluation && (matches!(self.peek(), Tok::Ident(_)) && !self.is_word("from") || self.is_word("from") && self.peek_at(1) != &Tok::Punct("'") && matches!(self.peek_at(1), Tok::Punct(",") | Tok::Ident(_))) {
            bindings.push((ImportName::Name("default".into()), self.binding_ident()?));
            more = self.eat(",");
        }
        if more {
            if self.eat("*") {
                self.expect_word("as")?;
                bindings.push((ImportName::Namespace, self.binding_ident()?));
            } else if phase == Phase::Defer {
                return self.err("expected `* as` after `import defer`");
            } else if self.eat("{") {
                while !self.eat("}") {
                    let name = self.module_export_name()?;
                    let local = if self.is_word("as") {
                        self.advance();
                        self.binding_ident()?
                    } else {
                        let ExportName::Ident(n) = &name else {
                            return self.err("expected `as` and a binding");
                        };
                        self.check_binding(n)?;
                        n.clone()
                    };
                    bindings.push((ImportName::Name(name.text()), local));
                    if !self.eat(",") {
                        self.expect("}")?;
                        break;
                    }
                }
            } else {
                return self.err("expected `*` or `{`");
            }
        }
        self.expect_word("from")?;
        let request = self.module_request_in(phase)?;
        self.semicolon()?;
        for (name, local) in bindings {
            self.import_binding(request, name, local);
        }
        Ok(Stmt { kind: StmtKind::Empty, pos })
    }

    /// Import binding `local` of `name` from `request`, declared.
    fn import_binding(&mut self, request: usize, name: ImportName, local: Name) {
        let local = self.declare(&local, IMPORT, 0);
        let k = self.info.imports.len();
        self.info.imports.push((request, name, local.clone()));
        let original = self.lexicals.get(&local).map_or(local.clone(), |l| l.original.clone());
        self.imports.insert(local, Import { original, index: k, holder: format!("%imp{k}").into(), prop: None });
    }

    /// Identifier `n` (just read) as a binding: not a reserved word (in
    /// strict code), `eval` or `arguments`.
    fn check_binding(&self, n: &Name) -> R<()> {
        self.strict_word(n)?;
        if is_reserved_word(n) || matches!(&**n, "eval" | "arguments") {
            return Err(Error::new(self.toks[self.i - 1].pos as usize, format!("`{n}` as a binding")));
        }
        Ok(())
    }

    /// Export name `name` (an early error if exported already).
    fn export_name(&mut self, name: &str, pos: Pos) -> R<()> {
        if self.exported.iter().any(|n| n == name) {
            return Err(Error::new(pos as usize, format!("`{name}` exported twice")));
        }
        self.exported.push(name.to_string());
        Ok(())
    }

    /// A variable's name as written (a block-scoped one is renamed).
    fn original_name(&self, n: &Name) -> String {
        self.lexicals.get(n).map_or_else(|| n.to_string(), |l| l.original.to_string())
    }

    /// `export …`: a declaration's statement (its names exported), or none.
    fn export_declaration(&mut self) -> R<Stmt<'a>> {
        let pos = self.pos();
        self.advance();
        let empty = Stmt { kind: StmtKind::Empty, pos };
        if self.eat("*") {
            let name = if self.is_word("as") {
                self.advance();
                Some(self.module_export_name()?.text())
            } else {
                None
            };
            self.expect_word("from")?;
            let request = self.module_request()?;
            self.semicolon()?;
            match name {
                Some(n) => {
                    self.export_name(&n, pos)?;
                    self.info.indirect_exports.push((n, request, ImportName::Namespace));
                }
                None => self.info.star_exports.push(request),
            }
            return Ok(empty);
        }
        if self.eat("{") {
            let mut specs = Vec::new();
            while !self.eat("}") {
                let at = self.pos();
                let local = self.module_export_name()?;
                let exported = if self.is_word("as") {
                    self.advance();
                    self.module_export_name()?.text()
                } else {
                    local.text()
                };
                specs.push((local, exported, at));
                if !self.eat(",") {
                    self.expect("}")?;
                    break;
                }
            }
            if self.is_word("from") {
                self.advance();
                let request = self.module_request()?;
                self.semicolon()?;
                for (local, exported, at) in specs {
                    self.export_name(&exported, at)?;
                    self.info.indirect_exports.push((exported, request, ImportName::Name(local.text())));
                }
            } else {
                self.semicolon()?;
                for (local, exported, at) in specs {
                    // (A reference: no string, no reserved word.)
                    let n = match local {
                        ExportName::Ident(n)
                            if !is_reserved_word(&n)
                                && !matches!(
                                    &*n,
                                    "implements" | "interface" | "package" | "private" | "protected" | "public" | "static" | "await"
                                ) =>
                        {
                            n
                        }
                        _ => return Err(Error::new(at as usize, "exported without `from`: not a variable's name".into())),
                    };
                    self.export_name(&exported, at)?;
                    let local = self.resolve(&n).unwrap_or(n.clone());
                    self.local_exports.push((exported, local, at));
                    let _ = n;
                }
            }
            return Ok(empty);
        }
        if self.eat("default") {
            return self.export_default(pos);
        }
        let declaration = match self.peek().clone() {
            Tok::Keyword("var" | "const" | "function" | "class") => true,
            Tok::Ident(n) => self.let_ahead(&n) || self.async_function_ahead(&n),
            _ => false,
        };
        if !declaration {
            return self.err("expected a declaration, `default`, `*` or `{` after `export`");
        }
        let s = self.statement()?;
        let names: Vec<Name> = match &s.kind {
            StmtKind::Var(ds) | StmtKind::Lexical(_, ds) => ds.iter().filter(|(n, _)| !n.is_empty()).map(|(n, _)| n.clone()).collect(),
            StmtKind::Function(i) => {
                let f = &self.functions[*i];
                f.binding.clone().or(f.name.clone()).into_iter().collect()
            }
            _ => Vec::new(),
        };
        for n in names {
            let exported = self.original_name(&n);
            self.export_name(&exported, pos)?;
            self.info.local_exports.push((exported, n.to_string()));
        }
        Ok(s)
    }

    /// `export default …`, past `default`: a function or class declaration
    /// (anonymous ones bound to `default`, a name no code can write), or
    /// an expression's value in a constant `default`.
    fn export_default(&mut self, pos: Pos) -> R<Stmt<'a>> {
        self.export_name("default", pos)?;
        let start = self.pos();
        let is_async = matches!(self.peek().clone(), Tok::Ident(n) if self.async_function_ahead(&n));
        if self.is("function") || is_async {
            let mut k = if is_async { 2 } else { 1 };
            if self.peek_at(k) == &Tok::Punct("*") {
                k += 1;
            }
            if matches!(self.peek_at(k), Tok::Ident(_)) {
                let s = self.statement()?;
                let StmtKind::Function(i) = s.kind else { unreachable!() };
                let f = &self.functions[i];
                let local = f.binding.clone().or(f.name.clone()).unwrap();
                self.info.local_exports.push(("default".into(), local.to_string()));
                return Ok(s);
            }
            if is_async {
                self.advance();
                self.pending_async = true;
            }
            self.advance();
            let f = self.function(false, false, start)?;
            let func = &mut self.functions[f];
            func.name = Some("default".into());
            func.declaration = true;
            self.body_function(f);
            self.info.local_exports.push(("default".into(), "default".into()));
            return Ok(Stmt { kind: StmtKind::Function(f), pos });
        }
        if self.eat("class") {
            let name = match self.peek() {
                Tok::Ident(_) => Some(self.ident()?),
                _ => None,
            };
            let c = self.class(name.clone(), start)?;
            let end = self.pos();
            let e = Expr { kind: ExprKind::Class(Box::new(c)), pos: start };
            let default: Name = "default".into();
            if name.is_none() {
                self.infer_name(&e, &default);
            }
            let binding = self.declare(name.as_ref().unwrap_or(&default), LET, end);
            self.info.local_exports.push(("default".into(), binding.to_string()));
            return Ok(Stmt { kind: StmtKind::Lexical(false, vec![(binding, Some(e))]), pos });
        }
        let e = self.assignment(false)?;
        self.semicolon()?;
        let default: Name = "default".into();
        self.infer_name(&e, &default);
        let end = self.pos();
        let binding = self.declare(&default, CONST, end);
        self.info.local_exports.push(("default".into(), binding.to_string()));
        Ok(Stmt { kind: StmtKind::Lexical(true, vec![(binding, Some(e))]), pos })
    }

    /// A module parsed (function `index`'s body): the early errors of its
    /// declarations (its functions are lexical, as its `let`s: none may
    /// share a name with another declaration), and its `export { x }`s,
    /// each of a variable it declares (an import's re-exported).
    fn check_module(&mut self, index: usize) -> R<()> {
        let vars = var_declared_names(&self.functions, index);
        let lexical: Vec<Name> = self.table.decls.get(&MODULE_SCOPE).map_or_else(Vec::new, |ds| ds.iter().map(|(n, _)| n.clone()).collect());
        let mut fns: Vec<Name> = Vec::new();
        for s in &self.functions[index].body {
            if let StmtKind::Function(i) = &s.kind {
                let f = &self.functions[*i];
                let n = f.name.clone().unwrap();
                if fns.contains(&n) || vars.contains(&n) || lexical.contains(&n) {
                    return Err(Error::new(f.pos as usize, format!("`{n}` has already been declared")));
                }
                fns.push(n);
            }
        }
        for (exported, local, at) in std::mem::take(&mut self.local_exports) {
            let original: Name = self.original_name(&local).into();
            if !vars.contains(&original) && !lexical.contains(&original) && !fns.contains(&original) {
                return Err(Error::new(at as usize, format!("`{original}` exported but not declared")));
            }
            match self.imports.get(&local) {
                // (A namespace's: re-exported as `export * as x` is; a
                // source's, as its source.)
                Some(imp) => {
                    let (request, name, _) = self.info.imports[imp.index].clone();
                    self.info.indirect_exports.push((exported, request, name));
                }
                None => self.info.local_exports.push((exported, local.to_string())),
            }
        }
        Ok(())
    }

    fn in_static_block(&self) -> bool {
        !self.naming_function && self.static_blocks.contains(&self.current)
    }

    fn static_block(&mut self, home_static: &Name) -> R<usize> {
        let key = self.i;
        let f = self.synthetic_method(home_static, self.pos());
        self.static_blocks.push(f);
        self.advance();
        self.expect("{")?;
        self.push_vars(key);
        let outer = std::mem::replace(&mut self.current, f);
        let body = self.scoped_block_rest();
        self.current = outer;
        self.frames.pop();
        self.functions[f].body = body?;
        self.functions[f].end = self.toks[self.i - 1].pos + 1;
        Ok(f)
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
                module_top: false,
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
            param_ends: Vec::new(),
            tdz_params: Vec::new(),
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
            fields: None,
            module: false,
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
            self.super_fns.push((ctor.clone(), self.current));
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
    let mut out = var_names_of(fs, f, true);
    let func = &fs[f];
    if !func.arrow {
        out.push("arguments".into());
    }
    if func.binds_name {
        out.extend(func.name.clone());
    }
    out
}

/// Function `f`'s parameters and `var`s, and (`functions`) the functions
/// declared in it.
fn var_names_of(fs: &[Function], f: usize, functions: bool) -> Vec<Name> {
    let func = &fs[f];
    let mut out: Vec<Name> = func.params.to_vec();
    func.body.iter().for_each(|s| declared_in(s, fs, &mut out, functions));
    out
}

/// The `var`s (and, `functions`, the functions) statement `s` declares.
fn declared_in(s: &Stmt, fs: &[Function], out: &mut Vec<Name>, functions: bool) {
    let walk = |s: &Stmt, out: &mut Vec<Name>| declared_in(s, fs, out, functions);
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
        StmtKind::Function(i) if functions => out.extend(fs[*i].name.clone()),
        _ => {}
    }
    match &s.kind {
        StmtKind::If(_, a, b) => {
            walk(a, out);
            if let Some(b) = b {
                walk(b, out);
            }
        }
        StmtKind::For { body, .. }
        | StmtKind::ForIn { body, .. }
        | StmtKind::ForOf { body, .. }
        | StmtKind::While(_, body)
        | StmtKind::DoWhile(body, _)
        | StmtKind::With(_, _, body)
        | StmtKind::Labeled(_, body) => walk(body, out),
        StmtKind::Block(b) => b.iter().for_each(|s| walk(s, out)),
        StmtKind::Switch(_, cases) => cases.iter().flat_map(|(_, b)| b).for_each(|s| walk(s, out)),
        StmtKind::Try(b, c, fin) => b
            .iter()
            .chain(c.iter().flat_map(|(_, c)| c))
            .chain(fin.iter().flatten())
            .for_each(|s| walk(s, out)),
        _ => {}
    }
}

/// A ModuleExportName (see `Parser::module_export_name`).
enum ExportName {
    Ident(Name),
    Reserved(Name),
    Str(String),
}

impl ExportName {
    fn text(&self) -> String {
        match self {
            ExportName::Ident(n) | ExportName::Reserved(n) => n.to_string(),
            ExportName::Str(s) => s.clone(),
        }
    }
}

/// Function `f`'s `var`s (not its parameters, nor the functions declared
/// in it).
fn var_declared_names(fs: &[Function], f: usize) -> Vec<Name> {
    let mut out = var_names_of(fs, f, false);
    let params = fs[f].params.len();
    out.drain(..params);
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
    Nullish,
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
        "??" => (1, Bin::Nullish),
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
        assert!(error("import x from \"y\";").contains("not at a module's top level"));
        assert!(error("f(").contains("end of input"));
        assert!(error("1 = 2").contains("assignment target"));
        assert!(error("return 1").contains("outside a function"));
        assert!(error("function (a) {}").contains("name"));
    }
}
