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
    /// The body's `var`s of functions with parameter expressions, renamed
    /// apart from the parameters' scope (`x%3`: see
    /// `parse::Parser::push_body_vars`): their names, by their new ones.
    /// A scope object has them by those names (its property keys), where
    /// a direct `eval` finds them.
    pub body_vars: std::collections::HashMap<Name, Name>,
    /// The parameters whose names their function's body declares again
    /// (see `parse::Parser::push_body_vars`), by function: in a scope
    /// object, a property of their own (`x%param`), the name the body's.
    pub shadowed_params: std::collections::HashSet<(usize, Name)>,
    /// Direct `eval`'s code: names are looked up in the caller's scope
    /// object first (the variable `%e0`, a `with`'s: see
    /// `intrinsics::direct_eval`).
    pub direct_eval: bool,
    /// Strict indirect `eval`'s code: its `var`s and functions are its
    /// own, not globals.
    pub eval_locals: bool,
    /// A module's imported bindings, by their new names (see
    /// [`Import`]).
    pub imports: std::collections::HashMap<Name, Import>,
    /// A module's requests, imports and exports (see [`ModuleInfo`]).
    pub module: Option<ModuleInfo>,
}

/// The name of a module's function (`functions[1]`, whose body is the
/// module's: see `parse::parse_module`).
pub const MODULE_FUNCTION: &str = "%module";
/// Its first parameter: the module's scope object, which its loader
/// makes (see `module`) and the function fills (see `codegen`).
pub const MODULE_ENV: &str = "%modenv";

/// An imported binding (`import { x as y } from "m"`): a reference to it
/// reads the variable it resolves to in the module exporting it, live, in
/// that module's scope object, which the module function gets as its
/// parameter `holder`; or with no `prop`, the parameter's value itself
/// (a namespace object). Assigning it throws.
#[derive(Debug, Clone)]
pub struct Import {
    /// Its name as written.
    pub original: Name,
    /// Its index in [`ModuleInfo::imports`].
    pub index: usize,
    /// The module function's parameter holding the scope object (or the
    /// namespace).
    pub holder: Name,
    /// The variable's name in that object, once linked (see
    /// `module::link`).
    pub prop: Option<String>,
}

/// A module request's phase: an ordinary one, `import defer`'s (the
/// module evaluated when its namespace is first read), or `import
/// source`'s (the module's source object, not its instance).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Phase {
    Evaluation,
    Defer,
    Source,
}

/// What an import or a re-export takes from a module: an export, its
/// namespace (a deferred one through a `Phase::Defer` request), or its
/// source object.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportName {
    Name(String),
    Namespace,
    Source,
}

/// A module's requests and its import and export entries (ECMA-262
/// §16.2.1.6), names as written, local ones as the module's variables'
/// (renamed: see [`Program::lexicals`]).
#[derive(Debug, Clone, Default)]
pub struct ModuleInfo {
    /// The modules it requests, in order, each once: the specifier, its
    /// `type` attribute (`with { type: "json" }`), if any, and the phase.
    pub requests: Vec<(String, Option<String>, Phase)>,
    /// `import … from`: the request, what is imported, the local binding
    /// (see [`Import`]).
    pub imports: Vec<(usize, ImportName, Name)>,
    /// `export`ed variables: the name exported, the variable.
    pub local_exports: Vec<(String, String)>,
    /// `export { x as y } from "m"`: the name exported, the request, what
    /// is imported (`export * as ns from "m"`: the namespace; an imported
    /// binding re-exported: the import's).
    pub indirect_exports: Vec<(String, usize, ImportName)>,
    /// `export * from "m"`: the requests.
    pub star_exports: Vec<usize>,
    /// It has a top-level `await` (its function is async).
    pub has_await: bool,
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
    /// Declared in a module's own scope (not a block in it): a variable
    /// of its scope object, which other modules may import (never in a
    /// box: see `codegen::boxed_lexicals`).
    pub module_top: bool,
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
    /// The statements at the start of its body that are its parameters'
    /// defaults and destructurings, each parameter's in turn.
    pub param_stmts: usize,
    /// Where each parameter ends (past its default): its statements are
    /// before, and it is initialized from there on.
    pub param_ends: Vec<Pos>,
    /// The names its parameters bind that a default or a pattern's may read
    /// before they are initialized (a parameter's own or a later one's, or
    /// any after a direct `eval`), with the parameter binding each: held
    /// uninitialized until their turn (a hole, read through `CheckHole`;
    /// see `codegen::Gen::param_tdz`).
    pub tdz_params: Vec<(Name, usize)>,
    /// Its last parameter is a rest parameter (`...r`), which collects the
    /// arguments from its position on.
    pub rest: bool,
    /// A class's constructor: 2 (a base class's), 3 (a derived class's);
    /// 0 otherwise.
    pub class_kind: u8,
    /// It reads `new.target` (`%newtarget`, a variable it sets at entry;
    /// an arrow's is the enclosing function's).
    pub uses_new_target: bool,
    /// Variables the parser adds to it (a class's home objects: see
    /// [`Class::home`]).
    pub hidden: Vec<Name>,
    /// Its source text's span when not `pos..end` (a class's constructor:
    /// the class's), for `Function.prototype.toString`.
    pub text: Option<(Pos, Pos)>,
    /// A class's method or constructor: the variable holding its `super`'s
    /// home object.
    pub home: Option<Name>,
    /// A class's constructor: the variable holding the class (whose
    /// prototype `super(…)` constructs).
    pub ctor_var: Option<Name>,
    /// The constructor of a class with instance fields (or private
    /// methods), or a function calling `super(…)` in a derived one's:
    /// the variable holding the function that initializes them on
    /// `this` (see [`Class::fields`]).
    pub fields: Option<Name>,
    /// A module's function (see [`MODULE_FUNCTION`]).
    pub module: bool,
    /// A generator function (`function*`, `*m() {}`).
    pub generator: bool,
    /// An async function (`async function`, `async () => …`, `async m()
    /// {}`): a coroutine as a generator's, driven by promises.
    pub is_async: bool,
    /// Its code calls `eval(…)` directly: its variables live in a scope
    /// object the eval'd code sees (see `codegen::generate_from`).
    pub has_eval: bool,
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

/// The name of the function the parser makes for a direct `eval`'s
/// bindings (see `Parser::eval_bindings`).
pub const EVAL_BINDINGS: &str = "%evalbindings";
/// What the parser tells of a binding a direct `eval` sees: a `const`, a
/// catch clause's parameter (which a `var` of the `eval`'s may redeclare),
/// or neither (0); and what the code generator adds: block-scoped (not an
/// enclosing function's variable), in a box (see
/// `codegen::boxed_lexicals`). (See `intrinsics::eval_scope`.)
pub const EVAL_CONST: u8 = 1;
pub const EVAL_CATCH: u8 = 2;
pub const EVAL_BLOCK_SCOPED: u8 = 4;
pub const EVAL_BOXED: u8 = 8;
/// An enclosing function's (past the `eval`'s function's own scope): a
/// `var` the `eval` declares shadows it, as a variable of the function's.
pub const EVAL_OUTER: u8 = 16;

/// The start of the name of the exception a statement list with `using`
/// declarations catches (see `Parser::lower_using`): its `try` statement
/// is the list's, which leaves the completion value as a block does.
pub const USING_CATCH: &str = "%usingcatch";

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
    /// (The loops' expressions in the arena: every statement takes as
    /// much as the largest kind, 96 bytes, 208 with these inline.)
    For {
        init: Option<ForInit<'a>>,
        test: Option<&'a Expr<'a>>,
        update: Option<&'a Expr<'a>>,
        body: &'a Stmt<'a>,
    },
    /// `for (var? x in e)`
    ForIn {
        var: bool,
        /// `for (let x in e)` or `const`: a binding per iteration.
        fresh: bool,
        name: Name,
        object: &'a Expr<'a>,
        body: &'a Stmt<'a>,
    },
    /// `for (target of iterable)`: `kind` 0 (an assignment target), 1
    /// (`var`), 2 (`let`) or 3 (`const`, a declaration's name renamed).
    ForOf {
        kind: u8,
        target: &'a Expr<'a>,
        /// A pattern target (then `target` is unused).
        pattern: Option<Box<Pattern<'a>>>,
        iterable: &'a Expr<'a>,
        body: &'a Stmt<'a>,
        /// `for await (…)`, in an async function: over an async iterator.
        is_await: bool,
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
    Expr(&'a Expr<'a>),
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
    /// Its properties, and the variable holding it if its methods' `super`
    /// refers to it (their home object).
    Object(Vec<(PropKey<'a>, Expr<'a>)>, Option<Name>),
    /// An array literal's elision (`[, 1]`).
    Hole,
    /// `...e`, in an array literal or arguments.
    Spread(&'a Expr<'a>),
    /// An untagged template literal: its strings (cooked) around its
    /// substitutions.
    Template(Box<[String]>, Vec<Expr<'a>>),
    /// A tagged template's first argument: the site's frozen array of
    /// cooked strings (`None`: an invalid escape), with its `raw` ones.
    TemplateObject(Vec<(Option<String>, String)>),
    /// `await e`, in an async function.
    Await(&'a Expr<'a>),
    /// `yield e` (`false`) or `yield* e` (`true`), in a generator.
    Yield(Option<&'a Expr<'a>>, bool),
    /// A class.
    Class(Box<Class<'a>>),
    /// `super(args)` in a derived class's constructor: the variable
    /// holding the class (see [`Class::home_static`]).
    SuperCall(Name, Vec<Expr<'a>>),
    /// `super[key]`: the variable holding the home object, the key, and
    /// `this`.
    SuperMember(Name, &'a Expr<'a>, &'a Expr<'a>),
    /// A destructuring of the value of the expression into the pattern
    /// (`mode` 0: assigning its targets, 1: initializing `var`s, 2:
    /// initializing block-scoped bindings or parameters, which may be
    /// held uninitialized until then); its value is the expression's.
    Destructure(Box<Pattern<'a>>, &'a Expr<'a>, u8),
    /// A declaration's name a destructuring in it sets (so not set to
    /// `undefined` itself).
    Bound,
    /// A runtime helper applied to its arguments (see
    /// `crate::intrinsics`), for what the parser desugars.
    Intrinsic(&'static str, Vec<Expr<'a>>),
    /// A function expression: an index into [`Program::functions`].
    Function(usize),
    /// A regular expression literal: pattern, flags.
    RegExp(Box<(String, String)>),
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
    /// `a ?? b`
    Nullish(&'a Expr<'a>, &'a Expr<'a>),
    Cond(&'a Expr<'a>, &'a Expr<'a>, &'a Expr<'a>),
    /// `target = value`, or `target op= value`.
    Assign(Option<BinOp>, &'a Expr<'a>, &'a Expr<'a>),
    /// `target &&= value` (0), `target ||= value` (1), `target ??= value`
    /// (2).
    LogicalAssign(u8, &'a Expr<'a>, &'a Expr<'a>),
    /// An optional chain (`a?.b.c`), `undefined` if any of its
    /// [`Optional`](ExprKind::Optional) parts is null or undefined.
    Chain(&'a Expr<'a>),
    /// In a [`Chain`](ExprKind::Chain), the value before a `?.`: its
    /// evaluation ends the chain if it's null or undefined.
    Optional(&'a Expr<'a>),
    Comma(&'a Expr<'a>, &'a Expr<'a>),
}

/// A class: its constructor (a function) and its methods, made where
/// the class is evaluated.
#[derive(Debug)]
pub struct Class<'a> {
    /// Its binding in its own body (renamed: see [`Program::lexicals`]).
    pub inner: Option<Name>,
    pub heritage: Option<Expr<'a>>,
    pub constructor: usize,
    pub members: Vec<ClassMember<'a>>,
    /// The variables (in the function the class is in) holding its
    /// prototype and itself: the home objects of its methods' and static
    /// methods' `super`.
    pub home: Name,
    pub home_static: Name,
    /// Its private names' variables (each holding a new private symbol
    /// when the class is evaluated), with their names (`#x`).
    pub privates: Vec<(Name, Name)>,
    /// The variable holding, and the function, defining its private
    /// methods and its fields on a new instance (called with it as
    /// `this`).
    pub fields: Option<(Name, usize)>,
    /// Functions run in order once its methods are defined, with the
    /// class as `this`: its static fields' and static blocks'.
    pub statics: Vec<usize>,
}

#[derive(Debug)]
pub struct ClassMember<'a> {
    pub key: PropKey<'a>,
    /// 0: a method; 1: a getter; 2: a setter; 3: a field's computed key,
    /// evaluated into variable `var` (`function` unused). A private one
    /// (`#m() {}`): its function into variable `var` (a static one's
    /// also defined on the class).
    pub kind: u8,
    pub is_static: bool,
    pub function: usize,
    pub var: Option<Name>,
}

/// A destructuring pattern.
#[derive(Debug)]
pub enum Pattern<'a> {
    Name(Name),
    /// An assignment pattern's member target (`o.x`, `o[k]`).
    Target(&'a Expr<'a>),
    /// Elements (`None`: an elision) with their defaults, and a rest
    /// element.
    Array(Vec<Option<(Pattern<'a>, Option<&'a Expr<'a>>)>>, Option<Box<Pattern<'a>>>),
    /// Properties with their defaults, and a rest property.
    Object(Vec<(PatKey<'a>, Pattern<'a>, Option<&'a Expr<'a>>)>, Option<Box<Pattern<'a>>>),
}

#[derive(Debug)]
pub enum PatKey<'a> {
    Name(Name),
    Computed(&'a Expr<'a>),
}

impl Pattern<'_> {
    /// The names it binds or assigns, in order.
    pub fn names(&self, out: &mut Vec<Name>) {
        match self {
            Pattern::Name(n) => out.push(n.clone()),
            Pattern::Target(_) => {}
            Pattern::Array(es, rest) => {
                es.iter().flatten().for_each(|(p, _)| p.names(out));
                if let Some(r) = rest {
                    r.names(out);
                }
            }
            Pattern::Object(ps, rest) => {
                ps.iter().for_each(|(_, p, _)| p.names(out));
                if let Some(r) = rest {
                    r.names(out);
                }
            }
        }
    }
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
