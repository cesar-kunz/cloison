//! The syntax tree. Every function, the script included, lives in
//! [`Program::functions`], and refers to the functions nested in it by
//! index, so each can be compiled on its own.

/// Byte offset into the source, for errors.
pub type Pos = u32;

/// A name (an identifier, a property name, a label): shared by every token
/// and node spelling it (the lexer makes one per distinct name).
pub type Name = std::rc::Rc<str>;

#[derive(Debug)]
pub struct Program<'a> {
    /// `functions[0]` is the script.
    pub functions: Vec<Function<'a>>,
    /// The block-scoped bindings (`let`, `const`, a function declared in
    /// a block, a catch parameter), each renamed apart (`x%3`: see
    /// `parse::Parser::reference`), by their new names.
    pub lexicals: std::collections::HashMap<Name, Lexical>,
}

/// A block-scoped binding (see [`Program::lexicals`]).
#[derive(Debug, Clone)]
pub struct Lexical {
    /// Its name as written.
    pub original: Name,
    pub constant: bool,
    /// Initialized where its scope starts (a function declaration, a
    /// catch parameter): never read uninitialized.
    pub hoisted: bool,
    /// Where its declaration ends: a reference past it in the same
    /// function reads it initialized (unless in a `switch`).
    pub end: Pos,
    /// The function declaring it.
    pub func: usize,
    /// Declared in a `switch`'s cases (which may jump past it).
    pub switch: bool,
    /// The `with` statements enclosing its declaration (see
    /// `Function::withs`): their objects don't shadow it.
    pub withs: usize,
}

/// Where a program's nodes live (see [`Arena`]).
#[derive(Default)]
pub struct Arenas<'a> {
    pub exprs: Arena<Expr<'a>>,
    pub stmts: Arena<Stmt<'a>>,
}

/// Values allocated together and freed together: in chunks that never
/// move once allocated (each filled up to its capacity), so a reference
/// into one lives as long as the arena. (No `Drop` of its own: a `Vec`'s
/// lets the values refer to each other.)
pub struct Arena<T> {
    chunks: std::cell::RefCell<Vec<Vec<T>>>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Arena {
            chunks: Default::default(),
        }
    }
}

impl<T> Arena<T> {
    pub fn alloc(&self, value: T) -> &T {
        let mut chunks = self.chunks.borrow_mut();
        if chunks.last().is_none_or(|c| c.len() == c.capacity()) {
            let n = chunks.last().map_or(64, |c| (c.capacity() * 2).min(4096));
            chunks.push(Vec::with_capacity(n));
        }
        let chunk = chunks.last_mut().unwrap();
        chunk.push(value);
        let at: *const T = chunk.last().unwrap();
        // The chunk never grows past its capacity, so the value stays
        // where it is until the arena is dropped.
        unsafe { &*at }
    }
}

#[derive(Debug)]
pub struct Function<'a> {
    /// The declared name, or the one inferred from what an anonymous
    /// function expression is assigned to (`C.prototype.m = function …`).
    pub name: Option<Name>,
    pub params: Vec<Name>,
    pub body: Vec<Stmt<'a>>,
    /// The enclosing function (`None` for the script).
    pub parent: Option<usize>,
    /// A function declaration (hoisted), as opposed to an expression.
    pub declaration: bool,
    /// A named function expression, whose name is bound in its body.
    pub binds_name: bool,
    /// Its body reads `arguments` (not counting nested functions').
    pub uses_arguments: bool,
    pub pos: Pos,
    /// Where its parameter list starts (its `(`) and its body ends (past
    /// its `}`): the text compiled again when compiled lazily (see
    /// `codegen::generate`).
    pub params_pos: Pos,
    pub end: Pos,
    /// A function expression right after a `(` or right before one: most
    /// such are called at once (`(function () { … })()`, a module's;
    /// `f = function () { … }()`), so compiled with the function around
    /// them, not lazily (V8's heuristic).
    pub eager: bool,
    /// Strict mode code (a `"use strict"` directive of its own or of an
    /// enclosing function): `this` isn't boxed or made the global object.
    pub strict: bool,
    /// The variables of the `with` statements enclosing it (see
    /// `StmtKind::With`), outermost first.
    pub withs: Vec<Name>,
    /// An arrow function: its `this` and `arguments` are the enclosing
    /// function's (the parser turns `this` into the variable `%this`,
    /// which that function sets: see `uses_this`).
    pub arrow: bool,
    /// An arrow function in it uses its `this`: the variable `%this` holds
    /// it (set at entry).
    pub uses_this: bool,
    /// A method (of an object literal or a class): not a constructor, and
    /// with no `prototype`.
    pub method: bool,
    /// Its `length`: the parameters before the first with a default or
    /// the rest parameter.
    pub length: usize,
    /// Its last parameter is a rest parameter (`...r`), which collects the
    /// arguments from its position on.
    pub rest: bool,
    /// A function declared in a block: its block-scoped binding (see
    /// [`Program::lexicals`]).
    pub binding: Option<Name>,
    /// Such a function's Annex B `var` binding (sloppy mode): assigned the
    /// function where its declaration is evaluated.
    pub annex_b: bool,
    /// Skimmed, not parsed (see `parse::parse_with`): no body, and the
    /// names it and the functions in it may refer to from outside it (at
    /// least those they do), which stand for its references.
    pub free: Option<Vec<Name>>,
}

#[derive(Debug)]
pub struct Stmt<'a> {
    pub kind: StmtKind<'a>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum StmtKind<'a> {
    Var(Vec<(Name, Option<Expr<'a>>)>),
    /// `let` (`false`) or `const` declarations, their names renamed (see
    /// [`Program::lexicals`]).
    Lexical(bool, Vec<(Name, Option<Expr<'a>>)>),
    /// A function declaration: an index into [`Program::functions`].
    Function(usize),
    Expr(Expr<'a>),
    If(Expr<'a>, &'a Stmt<'a>, Option<&'a Stmt<'a>>),
    For {
        init: Option<ForInit<'a>>,
        test: Option<Expr<'a>>,
        update: Option<Expr<'a>>,
        body: &'a Stmt<'a>,
    },
    /// `for (var? x in e)`
    ForIn {
        var: bool,
        /// `for (let x in e)` or `const`: a binding per iteration.
        fresh: bool,
        name: Name,
        object: Expr<'a>,
        body: &'a Stmt<'a>,
    },
    While(Expr<'a>, &'a Stmt<'a>),
    DoWhile(&'a Stmt<'a>, Expr<'a>),
    Return(Option<Expr<'a>>),
    Break(Option<Name>),
    Continue(Option<Name>),
    Throw(Expr<'a>),
    /// `try { … } catch (name) { … } finally { … }`, either of the last two
    /// optional.
    Try(Vec<Stmt<'a>>, Option<(Name, Vec<Stmt<'a>>)>, Option<Vec<Stmt<'a>>>),
    Block(Vec<Stmt<'a>>),
    /// The discriminant, then `case` tests (`None` for `default`) with
    /// their statements.
    Switch(Expr<'a>, Vec<(Option<Expr<'a>>, Vec<Stmt<'a>>)>),
    Labeled(Name, &'a Stmt<'a>),
    /// `with (object) body`: the object is kept in the variable named
    /// (`%with0`, …), which nested functions capture (see
    /// `codegen::Gen::with_lookup`).
    With(Expr<'a>, Name, &'a Stmt<'a>),
    Empty,
}

#[derive(Debug)]
pub enum ForInit<'a> {
    Var(Vec<(Name, Option<Expr<'a>>)>),
    /// `let` (`false`) or `const`: copied to a fresh binding per iteration.
    Lexical(bool, Vec<(Name, Option<Expr<'a>>)>),
    Expr(Expr<'a>),
}

#[derive(Debug)]
pub struct Expr<'a> {
    pub kind: ExprKind<'a>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum ExprKind<'a> {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
    Ident(Name),
    This,
    /// Its elements: `Hole` for an elision, `Spread` for `...e`.
    Array(Vec<Expr<'a>>),
    Object(Vec<(PropKey<'a>, Expr<'a>)>),
    /// An array literal's elision (`[, 1]`).
    Hole,
    /// `...e`, in an array literal or arguments.
    Spread(&'a Expr<'a>),
    /// An untagged template literal: its strings (cooked) around its
    /// substitutions.
    Template(Vec<String>, Vec<Expr<'a>>),
    /// A tagged template's first argument: the site's frozen array of
    /// cooked strings (`None`: an invalid escape), with its `raw` ones.
    TemplateObject(Vec<(Option<String>, String)>),
    /// A runtime helper applied to its arguments (see
    /// `crate::intrinsics`), for what the parser desugars.
    Intrinsic(&'static str, Vec<Expr<'a>>),
    /// A function expression: an index into [`Program::functions`].
    Function(usize),
    /// A regular expression literal: pattern, flags.
    RegExp(String, String),
    /// In an object literal only: `get k() {…}` (false) or `set k(v) {…}`
    /// (true), the function's index.
    Accessor(bool, usize),
    /// `o.name`
    Member(&'a Expr<'a>, Name),
    /// `o[key]`
    Index(&'a Expr<'a>, &'a Expr<'a>),
    Call(&'a Expr<'a>, Vec<Expr<'a>>),
    New(&'a Expr<'a>, Vec<Expr<'a>>),
    Unary(UnOp, &'a Expr<'a>),
    /// `++x`, `x--`, …: the target, `+1` or `-1`, and whether it's prefix.
    Update(&'a Expr<'a>, i8, bool),
    Binary(BinOp, &'a Expr<'a>, &'a Expr<'a>),
    /// `&&` (`true`) or `||` (`false`).
    Logical(bool, &'a Expr<'a>, &'a Expr<'a>),
    Cond(&'a Expr<'a>, &'a Expr<'a>, &'a Expr<'a>),
    /// `target = value`, or `target op= value`.
    Assign(Option<BinOp>, &'a Expr<'a>, &'a Expr<'a>),
    Comma(&'a Expr<'a>, &'a Expr<'a>),
}

/// An object literal property's key.
#[derive(Debug)]
pub enum PropKey<'a> {
    Name(Name),
    /// `[e]`
    Computed(Expr<'a>),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Plus,
    Not,
    BitNot,
    Typeof,
    Void,
    Delete,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
    Exp,
    Shl,
    Sar,
    Shr,
    BitAnd,
    BitOr,
    BitXor,
    Lt,
    Gt,
    Le,
    Ge,
    Eq,
    Ne,
    StrictEq,
    StrictNe,
    In,
    Instanceof,
}

impl BinOp {
    pub fn symbol(self) -> &'static str {
        use BinOp::*;
        match self {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod => "%",
            Exp => "**",
            Shl => "<<",
            Sar => ">>",
            Shr => ">>>",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            Eq => "==",
            Ne => "!=",
            StrictEq => "===",
            StrictNe => "!==",
            In => "in",
            Instanceof => "instanceof",
        }
    }
}
