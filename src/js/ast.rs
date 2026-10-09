//! The syntax tree. Every function, the script included, lives in
//! [`Program::functions`], and refers to the functions nested in it by
//! index, so each can be compiled on its own.

/// Byte offset into the source, for errors.
pub type Pos = u32;

#[derive(Debug)]
pub struct Program {
    /// `functions[0]` is the script.
    pub functions: Vec<Function>,
}

#[derive(Debug)]
pub struct Function {
    /// The declared name, or the one inferred from what an anonymous
    /// function expression is assigned to (`C.prototype.m = function …`).
    pub name: Option<String>,
    pub params: Vec<String>,
    pub body: Vec<Stmt>,
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
pub struct Stmt {
    pub kind: StmtKind,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum StmtKind {
    Var(Vec<(String, Option<Expr>)>),
    /// A function declaration: an index into [`Program::functions`].
    Function(usize),
    Expr(Expr),
    If(Expr, Box<Stmt>, Option<Box<Stmt>>),
    For {
        init: Option<ForInit>,
        test: Option<Expr>,
        update: Option<Expr>,
        body: Box<Stmt>,
    },
    /// `for (var? x in e)`
    ForIn {
        var: bool,
        name: String,
        object: Expr,
        body: Box<Stmt>,
    },
    While(Expr, Box<Stmt>),
    DoWhile(Box<Stmt>, Expr),
    Return(Option<Expr>),
    Break(Option<String>),
    Continue(Option<String>),
    Throw(Expr),
    /// `try { … } catch (name) { … } finally { … }`, either of the last two
    /// optional.
    Try(Vec<Stmt>, Option<(String, Vec<Stmt>)>, Option<Vec<Stmt>>),
    Block(Vec<Stmt>),
    /// The discriminant, then `case` tests (`None` for `default`) with
    /// their statements.
    Switch(Expr, Vec<(Option<Expr>, Vec<Stmt>)>),
    Labeled(String, Box<Stmt>),
    Empty,
}

#[derive(Debug)]
pub enum ForInit {
    Var(Vec<(String, Option<Expr>)>),
    Expr(Expr),
}

#[derive(Debug)]
pub struct Expr {
    pub kind: ExprKind,
    pub pos: Pos,
}

#[derive(Debug)]
pub enum ExprKind {
    Num(f64),
    Str(String),
    Bool(bool),
    Null,
    Ident(String),
    This,
    Array(Vec<Expr>),
    Object(Vec<(String, Expr)>),
    /// A function expression: an index into [`Program::functions`].
    Function(usize),
    /// `o.name`
    Member(Box<Expr>, String),
    /// `o[key]`
    Index(Box<Expr>, Box<Expr>),
    Call(Box<Expr>, Vec<Expr>),
    New(Box<Expr>, Vec<Expr>),
    Unary(UnOp, Box<Expr>),
    /// `++x`, `x--`, …: the target, `+1` or `-1`, and whether it's prefix.
    Update(Box<Expr>, i8, bool),
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// `&&` (`true`) or `||` (`false`).
    Logical(bool, Box<Expr>, Box<Expr>),
    Cond(Box<Expr>, Box<Expr>, Box<Expr>),
    /// `target = value`, or `target op= value`.
    Assign(Option<BinOp>, Box<Expr>, Box<Expr>),
    Comma(Box<Expr>, Box<Expr>),
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
