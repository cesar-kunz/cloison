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
}

#[derive(Debug)]
pub struct Stmt<'a> {
    pub kind: StmtKind<'a>,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum StmtKind<'a> {
    Var(Vec<(Name, Option<Expr<'a>>)>),
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
    Empty,
}

#[derive(Debug)]
pub enum ForInit<'a> {
    Var(Vec<(Name, Option<Expr<'a>>)>),
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
    Array(Vec<Expr<'a>>),
    Object(Vec<(Name, Expr<'a>)>),
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
