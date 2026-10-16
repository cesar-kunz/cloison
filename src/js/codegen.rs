//! Bytecode generation: a [`Program`] into a [`Module`], one function at a
//! time, following the conventions of `bench/bytecode/README.md`:
//!
//! - registers: `r0` is `this`, then the parameters, then the function's
//!   `var`s and function declarations in order of declaration, then
//!   temporaries, allocated as a stack;
//! - binary operators take the left operand in a register and the right
//!   one in the accumulator, or as an immediate (`…Smi`) if it's a literal;
//! - loops are rotated (the test at the bottom jumps back to the body), and
//!   skip the initial jump to the test when the first one is known to pass;
//! - within a basic block, a value already in the accumulator isn't loaded
//!   again.
//!
//! Variables captured by nested functions are not supported (there are no
//! context slots yet), and neither is anything the instruction set has no
//! opcode for: such programs are rejected with an error naming the
//! construct.

use crate::jit::{HashMap, HashSet};

use super::Error;
use super::ast::*;
use std::rc::Rc;

use crate::asm::{Constant, FunctionDef, LazyFn, Module};
use crate::bytecode::{Arg, Assembler, Label, Op};

pub fn generate(p: &Program, src: &str) -> Result<Module, Error> {
    generate_from(p, None, src).map(|(m, _)| m)
}

/// Compile only function `root` of `p` and leave the functions nested in
/// it for later: each a stub ([`LazyFn`]) that compiles at its first call
/// from its text in `source` (at the offsets `at` maps `p`'s to). Also
/// `root`'s upvalues, by name. (`eval`'s and `Function`'s code; their
/// functions mostly are never called: CodeLoad's.)
pub fn generate_lazy<'p>(
    p: &'p Program,
    root: usize,
    source: &Rc<str>,
    at: &dyn Fn(Pos) -> Pos,
    upvalues: &'p [Rc<str>],
) -> Result<Module, Error> {
    generate_from(p, Some((root, source, at, upvalues)), source).map(|(m, _)| m)
}

/// What [`generate_lazy`] fails with when a skimmed function (see
/// `Function::free`) can't wait: one whose upvalues would include a
/// scope object, or too many to declare again. Parsing in full then
/// leaves it out (see `js::compile_script_lazy`).
pub const SKIMMED_TOO_SOON: &str = "a skimmed function compiled now";

/// Upvalues past which a function compiles now: a lazily compiled one's
/// are declared again, as parameters (see `js::compile_lazy`).
const LAZY_UPVALUES: usize = 120;

/// What [`generate_from`] makes of a function.
#[derive(Clone, Copy, PartialEq)]
enum Emit {
    /// Nothing: an enclosing function compiles it.
    Skip,
    Compile,
    Stub,
}

/// `lazy`: compile only the root given, the functions in it stubs; with
/// the upvalues the root's stub had (no fewer than it uses).
#[allow(clippy::type_complexity)]
fn generate_from<'p>(
    p: &'p Program,
    lazy: Option<(usize, &Rc<str>, &dyn Fn(Pos) -> Pos, &'p [Rc<str>])>,
    src: &str,
) -> Result<(Module, Vec<&'p str>), Error> {
    let position = positions(src, lazy.map(|(_, _, at, _)| at));

    let mut decls: Vec<Vec<&str>> = (0..p.functions.len()).map(|i| declarations(p, i)).collect();
    // A function with more variables than registers keeps the last ones
    // in a scope object (`%env`, a register; see `Gen::function`).
    let mut overflow: Vec<Vec<&str>> = vec![Vec::new(); p.functions.len()];
    for i in 1..p.functions.len() {
        if decls[i].len() > LOCAL_REGS {
            let f = &p.functions[i];
            let params = f.params.len();
            let implicit: Vec<&str> = decls[i][params..]
                .iter()
                .copied()
                .filter(|&n| n == "arguments" || (f.binds_name && f.name.as_deref() == Some(n)))
                .collect();
            let mut d: Vec<&str> = decls[i][..params].to_vec();
            d.push(ENV);
            d.extend(implicit.iter().copied());
            d.extend(decls[i][params..].iter().copied().filter(|n| !implicit.contains(n)));
            overflow[i] = d.split_off(LOCAL_REGS);
            d.extend(overflow[i].iter().copied());
            decls[i] = d;
        }
    }
    let decls: Vec<Decls> = decls.into_iter().map(Decls::new).collect();
    let mut scopes = upvalues(p, &decls, &overflow);
    let names = Default::default();
    let root = lazy.map_or(0, |(r, ..)| r);
    // A stub's upvalues stand: they may be more than it uses, if it was
    // skimmed; its closures were made with them.
    if let Some((_, _, _, ups)) = lazy
        && root != 0
    {
        let ups: Vec<&str> = ups.iter().map(|n| &**n).collect();
        assert!(
            scopes.upvalues[root].iter().all(|n| ups.contains(n)),
            "a lazily compiled function's upvalues among its stub's"
        );
        scopes.upvalues[root] = ups;
    }
    // The functions in `root` (after it: the parser numbers them in
    // order), each compiled or a stub when its parent compiles; one whose
    // upvalues include a scope object (not a name to declare again: see
    // `js::compile_lazy`) or with too many parameters compiles now.
    let n = p.functions.len();
    let mut emit = vec![Emit::Skip; n];
    let mut index = vec![usize::MAX; n];
    let mut emitted = 0;
    for i in root..n {
        let f = &p.functions[i];
        emit[i] = match f.parent {
            _ if i == root => Emit::Compile,
            Some(k) if emit[k] == Emit::Compile => {
                if lazy.is_some()
                    && !f.eager
                    && f.withs.is_empty()
                    && !f.arrow
                    && !f.uses_this
                    && !scopes.upvalues[i].contains(&ENV)
                    // (Block-scoped ones' new names can't be declared again.)
                    && !scopes.upvalues[i].iter().any(|n| n.contains('%'))
                    && scopes.upvalues[i].len() <= LAZY_UPVALUES
                    && f.params.len() < LOCAL_REGS
                {
                    Emit::Stub
                } else if f.free.is_some() {
                    return Err(Error::new(f.pos as usize, SKIMMED_TOO_SOON.to_string()));
                } else {
                    Emit::Compile
                }
            }
            _ => Emit::Skip,
        };
        if emit[i] != Emit::Skip {
            index[i] = emitted;
            emitted += 1;
        }
    }
    let mut functions = Vec::with_capacity(emitted);
    for i in root..n {
        match emit[i] {
            Emit::Skip => {}
            Emit::Compile => {
                let mut f = Gen::new(p, &decls, &overflow[i], &scopes, &names, i, lazy.is_some()).function()?;
                f.closures.iter_mut().for_each(|c| *c = index[*c]);
                (f.line, f.col) = position(p.functions[i].params_pos);
                functions.push(f);
            }
            Emit::Stub => {
                let (_, source, at, _) = lazy.unwrap();
                let f = &p.functions[i];
                let nparams = f.params.len() as u32;
                functions.push(FunctionDef {
                    name: function_name(p, i),
                    nparams,
                    nregs: nparams + 1,
                    code: vec![Op::CompileLazy as u8],
                    ics: Vec::new(),
                    closures: Vec::new(),
                    constants: Vec::new(),
                    handlers: Vec::new(),
                    upvalues: upvalue_sources(p, &decls, &scopes, i),
                    captures: false,
                    strict: f.strict,
                    line: position(f.params_pos).0,
                    col: position(f.params_pos).1,
                    lazy: Some(LazyFn {
                        source: source.clone(),
                        start: at(f.params_pos),
                        end: at(f.end),
                        own_name: f.name.clone().filter(|_| f.binds_name),
                        upvalues: scopes.upvalues[i].iter().map(|&n| n.into()).collect(),
                        strict: f.strict,
                    }),
                    kind: function_kind(f),
                    length: f.length as u32,
                });
            }
        }
    }
    let declared = if root == 0 {
        explicit_declarations(p, 0)
            .iter()
            .filter(|n| !p.lexicals.contains_key(**n))
            .map(|s| s.to_string())
            .collect()
    } else {
        Vec::new()
    };
    let lexical = if root == 0 {
        p.functions[0]
            .body
            .iter()
            .flat_map(|s| match &s.kind {
                StmtKind::Lexical(_, ds) => ds.iter().map(|(n, _)| original(n).to_string()).collect(),
                _ => Vec::new(),
            })
            .collect()
    } else {
        Vec::new()
    };
    let module = Module {
        lexical,
        functions,
        script: 0,
        declared,
    };
    Ok((module, std::mem::take(&mut scopes.upvalues[root])))
}

type R<T> = Result<T, Error>;

/// A function's declarations (see [`declarations`]), in order, and past a
/// few, where each is (the first of its name), for lookups by name: every
/// global's reference looks in each enclosing function's.
struct Decls<'a> {
    list: Vec<&'a str>,
    index: Option<HashMap<&'a str, usize>>,
}

impl<'a> Decls<'a> {
    fn new(list: Vec<&'a str>) -> Self {
        let index = (list.len() > 8).then(|| {
            let mut m = HashMap::default();
            for (i, &n) in list.iter().enumerate() {
                m.entry(n).or_insert(i);
            }
            m
        });
        Decls { list, index }
    }

    fn position(&self, name: &str) -> Option<usize> {
        match &self.index {
            Some(m) => m.get(name).copied(),
            None => self.list.iter().position(|&n| n == name),
        }
    }

    fn contains(&self, name: &&str) -> bool {
        self.position(name).is_some()
    }
}

impl<'a> std::ops::Deref for Decls<'a> {
    type Target = [&'a str];

    fn deref(&self) -> &[&'a str] {
        &self.list
    }
}

/// A function's parameters and variables in registers, at most; the rest
/// go in a scope object (see [`generate`]), leaving room for temporaries.
const LOCAL_REGS: usize = 160;

/// Inline-cache slots past which instructions share them (see `Gen::ic`):
/// below 2^16, with room for the shared ones.
const SHARED_ICS: usize = 60000;

/// The name of the register holding a function's scope object.
const ENV: &str = "%env";

/// Function `i`'s name, for its code.
/// Where a position of the program is in `src` (through `at`, for a
/// function compiled lazily), as 1-based line and column (bytes), for
/// `TOI_JIT_LOG` and `TOI_JIT_DUMP_FN` (`scripts/vs-maglev.sh` matches
/// functions by it with V8's, which places them at their parameter list's
/// `(` too). Without either, (0, 0): finding lines scans the source, which
/// a lazy compile would do again for every function.
fn positions<'a>(src: &'a str, at: Option<&'a dyn Fn(Pos) -> Pos>) -> impl Fn(Pos) -> (u32, u32) + 'a {
    static LOGGING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let logging = *LOGGING.get_or_init(|| {
        std::env::var_os("TOI_JIT_LOG").is_some() || std::env::var_os("TOI_JIT_DUMP_FN").is_some()
    });
    let starts: Vec<usize> = if logging {
        std::iter::once(0).chain(src.match_indices('\n').map(|(k, _)| k + 1)).collect()
    } else {
        Vec::new()
    };
    move |pos| {
        if starts.is_empty() {
            return (0, 0);
        }
        let off = at.map_or(pos, |at| at(pos)) as usize;
        let line = starts.partition_point(|&s| s <= off);
        (line as u32, (off - starts[line - 1] + 1) as u32)
    }
}

fn function_name(p: &Program, i: usize) -> String {
    match (i, &p.functions[i].name) {
        (0, _) => "<script>".to_string(),
        (_, Some(n)) => n.to_string(),
        (_, None) => "<anonymous>".to_string(),
    }
}

/// Where each of function `i`'s upvalues comes from, in its parent: a
/// register of the parent's (`true`), or an upvalue of its own.
fn upvalue_sources(p: &Program, decls: &[Decls], scopes: &Scopes, i: usize) -> Vec<(bool, u8)> {
    let Some(parent) = p.functions[i].parent else {
        return Vec::new();
    };
    scopes.upvalues[i]
        .iter()
        .map(|n| match decls[parent].position(n) {
            Some(k) if parent != 0 || p.lexicals.contains_key(*n) => (true, k as u8 + 1),
            _ => {
                let k = scopes.upvalues[parent].iter().position(|d| d == n);
                (false, k.expect("passed down by the parent") as u8)
            }
        })
        .collect()
}

/// A variable's name as written (a block-scoped one is renamed `x%3`:
/// see `ast::Program::lexicals`).
fn original(name: &str) -> &str {
    match name.find('%') {
        Some(k) if k > 0 => &name[..k],
        _ => name,
    }
}

/// `vm::Code::kind` for `f`.
fn function_kind(f: &Function) -> u8 {
    if f.arrow || f.method { 1 } else { 0 }
}

fn unsupported<T>(pos: Pos, what: impl std::fmt::Display) -> R<T> {
    Err(Error::new(pos as usize, format!("unsupported: {what}")))
}

/// Per function, the variables of enclosing functions it uses (its
/// upvalues, in order; for a function between the one declaring a
/// variable and one using it, too, to pass it down), and its own variables
/// that nested functions use (captured). The script's are globals: never
/// upvalues.
struct Scopes<'a> {
    upvalues: Vec<Vec<&'a str>>,
    captured: Vec<Vec<&'a str>>,
}

fn upvalues<'a>(p: &'a Program, decls: &[Decls<'a>], overflow: &[Vec<&'a str>]) -> Scopes<'a> {
    let n = p.functions.len();
    let (mut upvalues, mut captured) = (vec![Vec::new(); n], vec![Vec::new(); n]);
    for i in 1..n {
        for name in references(&p.functions[i]) {
            // An enclosing function's variable in its scope object: the
            // object's register is what is captured.
            let mut name = name;
            let mut j = p.functions[i].parent;
            while let Some(k) = j.filter(|&k| k != 0) {
                if decls[i].contains(&name) {
                    break;
                }
                if decls[k].contains(&name) {
                    if overflow[k].contains(&name) {
                        name = ENV;
                    }
                    break;
                }
                j = p.functions[k].parent;
            }
            if decls[i].contains(&name) {
                continue;
            }
            let (mut chain, mut j) = (vec![i], p.functions[i].parent);
            let lexical = p.lexicals.contains_key(name);
            let owner = loop {
                match j {
                    Some(k) if k != 0 || lexical => {
                        if decls[k].contains(&name) {
                            break Some(k);
                        }
                        chain.push(k);
                        j = p.functions[k].parent;
                    }
                    _ => break None,
                }
            };
            if let Some(k) = owner {
                for c in chain {
                    if !upvalues[c].contains(&name) {
                        upvalues[c].push(name);
                    }
                }
                if !captured[k].contains(&name) {
                    captured[k].push(name);
                }
            }
        }
    }
    Scopes { upvalues, captured }
}

/// The names expression `e` refers to (not its nested functions').
fn expr_refs<'a>(e: &'a Expr, out: &mut Vec<&'a str>) {
    expr_names(e, out, false)
}

/// The names expression `e` refers to, or with `forwarded`, only its
/// `arguments` that `g.apply(x, arguments)` passes on (see
/// [`forwards_arguments`]).
fn expr_names<'a>(e: &'a Expr, out: &mut Vec<&'a str>, forwarded: bool) {
    use ExprKind::*;
    let expr_refs = |e: &'a Expr, out: &mut Vec<&'a str>| expr_names(e, out, forwarded);
    match &e.kind {
        Ident(n) if !forwarded => out.push(n),
        Ident(_) => {}
        Call(f, args) if forwarded && apply_arguments(f, args) => {
            out.push("arguments");
            expr_refs(f, out);
            expr_refs(&args[0], out);
        }
        Num(_) | Str(_) | Bool(_) | Null | This | Function(_) | Accessor(..) | RegExp(..) | Hole
        | TemplateObject(_) => {}
        Array(es) | Template(_, es) | Intrinsic(_, es) => es.iter().for_each(|e| expr_refs(e, out)),
        Object(ps) => ps.iter().for_each(|(k, e)| {
            if let PropKey::Computed(k) = k {
                expr_refs(k, out);
            }
            expr_refs(e, out)
        }),
        Member(o, _) | Unary(_, o) | Update(o, ..) | Spread(o) => expr_refs(o, out),
        Index(a, b) | Binary(_, a, b) | Logical(_, a, b) | Comma(a, b) | Assign(_, a, b) => {
            expr_refs(a, out);
            expr_refs(b, out);
        }
        Call(f, args) | New(f, args) => {
            expr_refs(f, out);
            args.iter().for_each(|e| expr_refs(e, out));
        }
        Cond(a, b, c) => [a, b, c].into_iter().for_each(|e| expr_refs(e, out)),
    }
}

/// The names function `f`'s own code refers to (not its nested
/// functions').
fn references<'a>(f: &'a Function<'_>) -> Vec<&'a str> {
    let mut out = match &f.free {
        Some(free) => free.iter().map(|n| &**n).collect(),
        None => names(f, false),
    };
    // (The objects of the `with` statements enclosing it: captured.)
    out.extend(f.withs.iter().map(|n| &**n));
    out
}

/// Whether call `f(args)` is `g.apply(x, arguments)`.
fn apply_arguments(f: &Expr, args: &[Expr]) -> bool {
    matches!(&f.kind, ExprKind::Member(_, m) if &**m == "apply")
        && matches!(args, [_, a] if matches!(&a.kind, ExprKind::Ident(n) if &**n == "arguments"))
}

/// The names function `f`'s own code refers to, or with `forwarded`, the
/// `arguments` it passes on (see [`expr_names`]).
fn names<'a>(f: &'a Function<'_>, forwarded: bool) -> Vec<&'a str> {
    fn stmt<'a>(s: &'a Stmt, out: &mut Vec<&'a str>, fw: bool) {
        let expr_refs = |e: &'a Expr, out: &mut Vec<&'a str>| expr_names(e, out, fw);
        let stmt = |s: &'a Stmt, out: &mut Vec<&'a str>| stmt(s, out, fw);
        match &s.kind {
            StmtKind::Var(ds) | StmtKind::Lexical(_, ds) => {
                ds.iter().flat_map(|(_, e)| e).for_each(|e| expr_refs(e, out))
            }
            StmtKind::Function(_)
            | StmtKind::Empty
            | StmtKind::Break(_)
            | StmtKind::Continue(_) => {}
            StmtKind::Expr(e) | StmtKind::Throw(e) => expr_refs(e, out),
            StmtKind::Return(e) => e.iter().for_each(|e| expr_refs(e, out)),
            StmtKind::If(c, a, b) => {
                expr_refs(c, out);
                stmt(a, out);
                b.iter().for_each(|b| stmt(b, out));
            }
            StmtKind::For {
                init,
                test,
                update,
                body,
            } => {
                match init {
                    Some(ForInit::Var(ds) | ForInit::Lexical(_, ds)) => {
                        ds.iter().flat_map(|(_, e)| e).for_each(|e| expr_refs(e, out))
                    }
                    Some(ForInit::Expr(e)) => expr_refs(e, out),
                    None => {}
                }
                test.iter().chain(update).for_each(|e| expr_refs(e, out));
                stmt(body, out);
            }
            StmtKind::ForIn {
                name, object, body, ..
            } => {
                if !fw {
                    out.push(name);
                }
                expr_refs(object, out);
                stmt(body, out);
            }
            StmtKind::While(c, body) | StmtKind::DoWhile(body, c) | StmtKind::With(c, _, body) => {
                expr_refs(c, out);
                stmt(body, out);
            }
            StmtKind::Block(b) => b.iter().for_each(|s| stmt(s, out)),
            StmtKind::Switch(d, cases) => {
                expr_refs(d, out);
                for (t, b) in cases {
                    t.iter().for_each(|t| expr_refs(t, out));
                    b.iter().for_each(|s| stmt(s, out));
                }
            }
            StmtKind::Labeled(_, body) => stmt(body, out),
            StmtKind::Try(b, c, f) => try_blocks(b, c, f).for_each(|s| stmt(s, out)),
        }
    }
    let mut out = Vec::new();
    f.body.iter().for_each(|s| stmt(s, &mut out, forwarded));
    out
}

/// Every name a function declares: parameters, `var`s and function
/// declarations, in order of declaration.
fn declarations<'a>(p: &'a Program<'_>, f: usize) -> Vec<&'a str> {
    let mut out = explicit_declarations(p, f);
    // The script's variables are globals, but for its block-scoped ones.
    if f == 0 {
        out.retain(|n| p.lexicals.contains_key(*n));
        return out;
    }
    // `arguments`: a variable `CreateArguments` sets.
    if arguments_object(p, f) && !out.contains(&"arguments") {
        out.push("arguments");
    }
    // `%this`: the function's `this`, for the arrows in it.
    if p.functions[f].uses_this && f != 0 {
        out.push("%this");
    }
    // A named function expression's own name, if used: a variable set to
    // the function (`LdaCallee`).
    if let Some(n) = own_name(p, f)
        && !out.contains(&n)
    {
        out.push(n);
    }
    out
}

/// The parameters and the variables and functions a function declares.
fn explicit_declarations<'a>(p: &'a Program<'_>, f: usize) -> Vec<&'a str> {
    fn stmt<'a>(s: &'a Stmt, p: &'a Program, out: &mut Vec<&'a str>) {
        let add = |n: &'a str, out: &mut Vec<&'a str>| {
            if !out.contains(&n) {
                out.push(n)
            }
        };
        match &s.kind {
            StmtKind::Var(ds)
            | StmtKind::Lexical(_, ds)
            | StmtKind::For {
                init: Some(ForInit::Var(ds) | ForInit::Lexical(_, ds)),
                ..
            } => ds.iter().for_each(|(n, _)| add(n, out)),
            StmtKind::Function(i) => {
                let f = &p.functions[*i];
                if let Some(b) = &f.binding {
                    add(b, out);
                }
                if f.binding.is_none() || f.annex_b {
                    add(f.name.as_deref().unwrap(), out)
                }
            }
            StmtKind::ForIn { var, fresh, name, .. } if *var || *fresh => add(name, out),
            StmtKind::Try(_, Some((name, _)), _) if p.lexicals.contains_key(name) => add(name, out),
            _ => {}
        }
        match &s.kind {
            StmtKind::If(_, a, b) => {
                stmt(a, p, out);
                if let Some(b) = b {
                    stmt(b, p, out);
                }
            }
            StmtKind::With(_, name, body) => {
                add(name, out);
                stmt(body, p, out)
            }
            StmtKind::For { body, .. }
            | StmtKind::ForIn { body, .. }
            | StmtKind::While(_, body)
            | StmtKind::DoWhile(body, _)
            | StmtKind::Labeled(_, body) => stmt(body, p, out),
            StmtKind::Block(b) => b.iter().for_each(|s| stmt(s, p, out)),
            StmtKind::Switch(_, cases) => cases
                .iter()
                .flat_map(|(_, b)| b)
                .for_each(|s| stmt(s, p, out)),
            StmtKind::Try(b, c, f) => try_blocks(b, c, f).for_each(|s| stmt(s, p, out)),
            _ => {}
        }
    }
    let f = &p.functions[f];
    let mut out: Vec<&str> = f.params.iter().map(|p| &**p).collect();
    for s in &f.body {
        stmt(s, p, &mut out);
    }
    out
}

/// The name a named function expression `f` binds in its body, if it or a
/// function nested in it refers to it (shadowed or not: then the variable
/// is just unused).
fn own_name<'a>(p: &'a Program<'_>, f: usize) -> Option<&'a str> {
    let func = &p.functions[f];
    let name = func.name.as_deref().filter(|_| func.binds_name)?;
    let within = |mut g: usize| loop {
        if g == f {
            return true;
        }
        match p.functions[g].parent {
            Some(q) => g = q,
            None => return false,
        }
    };
    (0..p.functions.len())
        .any(|g| within(g) && references(&p.functions[g]).contains(&name))
        .then_some(name)
}

/// Whether function `f` gets an `arguments` object: it reads `arguments`,
/// no parameter has that name, and it doesn't just pass them on (see
/// [`forwards_arguments`]).
fn arguments_object(p: &Program, f: usize) -> bool {
    let func = &p.functions[f];
    func.uses_arguments
        && func.parent.is_some()
        && !func.params.iter().any(|n| &**n == "arguments")
        && !forwards_arguments(p, f)
}

/// Whether function `f` reads `arguments` only to pass them on, as
/// `g.apply(x, arguments)` (a class's constructor calling its
/// `initialize`, in Prototype's style), and declares no variable of the
/// name: then it makes no `arguments` object, and those calls are
/// `CallApplyArguments`, which pass the frame's arguments themselves
/// (the JIT compiles them; it doesn't compile `CreateArguments`).
fn forwards_arguments(p: &Program, f: usize) -> bool {
    let func = &p.functions[f];
    let count = |forwarded| names(func, forwarded).iter().filter(|&&n| n == "arguments").count();
    func.uses_arguments
        && func.parent.is_some()
        && !explicit_declarations(p, f).contains(&"arguments")
        // (None of its own: an arrow in it reads them.)
        && count(false) > 0
        && count(true) == count(false)
}

/// The function declarations directly in a function's body (hoisted to
/// its start), in order. One nested in a block is Annex B's: its `var`
/// binding is assigned where the declaration is (see `Gen::stmt`).
fn hoisted(s: &Stmt, out: &mut Vec<usize>) {
    if let StmtKind::Function(i) = &s.kind {
        out.push(*i);
    }
}

/// The statements of a `try`'s blocks.
fn try_blocks<'a>(
    b: &'a [Stmt],
    c: &'a Option<(Name, Vec<Stmt>)>,
    f: &'a Option<Vec<Stmt>>,
) -> impl Iterator<Item = &'a Stmt<'a>> {
    b.iter()
        .chain(c.iter().flat_map(|(_, c)| c))
        .chain(f.iter().flatten())
}

/// An int32 literal (`-0` is not one).
fn smi(e: &Expr) -> Option<i32> {
    let v = match &e.kind {
        ExprKind::Num(v) => *v,
        ExprKind::Unary(UnOp::Neg, x) => match x.kind {
            ExprKind::Num(v) => -v,
            _ => return None,
        },
        _ => return None,
    };
    let i = v as i32;
    (i as f64 == v && !(v == 0.0 && v.is_sign_negative())).then_some(i)
}

/// Whether evaluating `e` may assign the variable `name`.
fn assigns(e: &Expr, name: &str) -> bool {
    use ExprKind::*;
    match &e.kind {
        Assign(_, t, v) => {
            matches!(&t.kind, Ident(n) if &**n == name) || assigns(t, name) || assigns(v, name)
        }
        Update(t, ..) => matches!(&t.kind, Ident(n) if &**n == name) || assigns(t, name),
        Num(_) | Str(_) | Bool(_) | Null | Ident(_) | This | Function(_) | Accessor(..) | RegExp(..) | Hole
        | TemplateObject(_) => false,
        Array(es) | Template(_, es) | Intrinsic(_, es) => es.iter().any(|e| assigns(e, name)),
        Object(ps) => ps
            .iter()
            .any(|(k, e)| matches!(k, PropKey::Computed(k) if assigns(k, name)) || assigns(e, name)),
        Member(o, _) | Spread(o) => assigns(o, name),
        Unary(_, x) => assigns(x, name),
        Index(a, b) | Binary(_, a, b) | Logical(_, a, b) | Comma(a, b) => {
            assigns(a, name) || assigns(b, name)
        }
        Call(f, args) | New(f, args) => assigns(f, name) || args.iter().any(|e| assigns(e, name)),
        Cond(a, b, c) => assigns(a, name) || assigns(b, name) || assigns(c, name),
    }
}

/// Whether `e`'s value is always a boolean.
fn is_boolean(e: &Expr) -> bool {
    use BinOp::*;
    match &e.kind {
        ExprKind::Bool(_) | ExprKind::Unary(UnOp::Not, _) => true,
        ExprKind::Binary(op, ..) => matches!(
            op,
            Lt | Gt | Le | Ge | Eq | Ne | StrictEq | StrictNe | In | Instanceof
        ),
        _ => false,
    }
}

/// Whether `s` always ends in a `return` and has no `break` or `continue`
/// (so it can be compiled anywhere in the function).
fn returns(s: &Stmt) -> bool {
    fn jumps(s: &Stmt) -> bool {
        match &s.kind {
            StmtKind::Break(_) | StmtKind::Continue(_) => true,
            StmtKind::If(_, a, b) => jumps(a) || b.as_deref().is_some_and(jumps),
            StmtKind::For { body, .. }
            | StmtKind::ForIn { body, .. }
            | StmtKind::While(_, body)
            | StmtKind::DoWhile(body, _)
            | StmtKind::With(_, _, body)
            | StmtKind::Labeled(_, body) => jumps(body),
            StmtKind::Block(b) => b.iter().any(jumps),
            StmtKind::Switch(_, cases) => cases.iter().flat_map(|(_, b)| b).any(jumps),
            StmtKind::Try(b, c, f) => try_blocks(b, c, f).any(jumps),
            _ => false,
        }
    }
    let last = match &s.kind {
        StmtKind::Block(b) => match b.last() {
            Some(l) => matches!(l.kind, StmtKind::Return(_)),
            None => false,
        },
        k => matches!(k, StmtKind::Return(_)),
    };
    last && !jumps(s)
}

/// Where a name refers to.
enum Var {
    Reg(u8),
    /// A property of the scope object in this register.
    Scope(u8),
    /// A property of an enclosing function's scope object, that object
    /// this function's upvalue.
    ScopeUpvalue(u8),
    /// An enclosing function's variable: upvalue `k` of this one's.
    Upvalue(u8),
    Global,
    Undefined,
}

/// Which outcome of a test falls through to the next instruction (the
/// other one jumps).
#[derive(Clone, Copy, PartialEq, Eq)]
enum Fall {
    True,
    False,
}

impl Fall {
    fn flip(self) -> Fall {
        match self {
            Fall::True => Fall::False,
            Fall::False => Fall::True,
        }
    }
}

/// A conditional jump that has a negation.
#[derive(Clone, Copy)]
enum Jmp {
    True,
    False,
    ToBooleanTrue,
    ToBooleanFalse,
}

impl Jmp {
    fn op(self) -> Op {
        match self {
            Jmp::True => Op::JumpIfTrue,
            Jmp::False => Op::JumpIfFalse,
            Jmp::ToBooleanTrue => Op::JumpIfToBooleanTrue,
            Jmp::ToBooleanFalse => Op::JumpIfToBooleanFalse,
        }
    }

    fn not(self) -> Jmp {
        match self {
            Jmp::True => Jmp::False,
            Jmp::False => Jmp::True,
            Jmp::ToBooleanTrue => Jmp::ToBooleanFalse,
            Jmp::ToBooleanFalse => Jmp::ToBooleanTrue,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
struct L(usize);

struct LabelInfo {
    label: Label,
    /// A jump to it has been emitted.
    used: bool,
    bound: bool,
    /// Bound before the jumps to it (a loop header).
    backward: bool,
}

/// A statement `break` or `continue` can leave.
struct Target {
    labels: Vec<Name>,
    brk: L,
    /// For loops.
    cont: Option<L>,
    /// A loop or a `switch`: an unlabeled `break` leaves it.
    breakable: bool,
}

/// A set of registers.
#[derive(Default, Clone, Copy)]
struct RegBits([u64; 4]);

impl RegBits {
    fn one(r: u8) -> RegBits {
        let mut s = RegBits::default();
        s.insert(r);
        s
    }
    fn insert(&mut self, r: u8) {
        self.0[r as usize / 64] |= 1 << (r % 64);
    }
    fn remove(&mut self, r: u8) {
        self.0[r as usize / 64] &= !(1 << (r % 64));
    }
    fn contains(&self, r: u8) -> bool {
        self.0[r as usize / 64] & 1 << (r % 64) != 0
    }
}

/// What the accumulator is known to hold, within a basic block.
#[derive(Default, Clone)]
struct Acc {
    /// Registers holding the same value.
    regs: RegBits,
    smi: Option<i32>,
}

struct Gen<'a> {
    p: &'a Program<'a>,
    decls: &'a [Decls<'a>],
    scopes: &'a Scopes<'a>,
    index: usize,
    f: &'a Function<'a>,
    asm: Assembler,
    labels: Vec<LabelInfo>,
    ics: Vec<Name>,
    /// The names of the module's caches, each once (see [`Gen::ic`]).
    names: &'a std::cell::RefCell<HashSet<Name>>,
    closures: Vec<usize>,
    constants: Vec<Constant>,
    nregs: usize,
    locals: HashMap<&'a str, u8>,
    /// Shared inline-cache slots (see [`Gen::ic`]), by kind and name.
    shared_ics: HashMap<String, u16>,
    /// Variables in the scope object (see [`generate`]).
    overflow: &'a [&'a str],
    /// The next free temporary.
    temp: usize,
    acc: Acc,
    reachable: bool,
    targets: Vec<Target>,
    /// Labels for the statement being compiled (`a: for …`).
    pending_labels: Vec<Name>,
    /// Statements compiled at the end of the function: the `then` branches
    /// of `if`s whose condition can only jump when true.
    deferred: Vec<(L, &'a Stmt<'a>)>,
    /// Exception handlers: (start, end, handler) bytecode offsets, inner
    /// ones first (see `FunctionDef::handlers`).
    handlers: Vec<(u32, u32, u32)>,
    /// `try` blocks being compiled: none of their statements goes out of
    /// line, outside the handler's range.
    in_try: u32,
    /// The `try` statements being compiled, outermost first.
    trys: Vec<TryCtx<'a>>,
    /// The variables holding the objects of the `with` statements
    /// enclosing the code being compiled, outermost first: names are
    /// looked up in them first (see [`Gen::with_lookup`]).
    withs: Vec<Name>,
    /// Compiling `eval`'s or `Function`'s code (see [`generate_lazy`]).
    eval: bool,
    /// The script's completion value (`eval`'s result): a register,
    /// in the script only.
    completion: Option<u8>,
    /// The statement being compiled has cleared the completion value
    /// (so its own match arm doesn't again).
    cleared: bool,
    /// How many statements enclose the one being compiled: 1 for the
    /// function body's own.
    depth: u32,
    /// A `for (let …)` loop's captured variables, for [`Gen::looping`] to
    /// give each iteration its own.
    fresh: Vec<&'a str>,
    /// Compiling the function's entry (see [`Gen::fresh_binding`]).
    entering: bool,
}

/// A `try` being compiled: the bytecode ranges it covers (closed around
/// the copies of `finally` blocks that jumps out of it run), its
/// `finally` block, and how many jump targets enclose it.
struct TryCtx<'a> {
    start: u32,
    ranges: Vec<(u32, u32)>,
    finally: Option<&'a [Stmt<'a>]>,
    targets: usize,
}

impl<'a> Gen<'a> {
    fn new(
        p: &'a Program<'a>,
        decls: &'a [Decls<'a>],
        overflow: &'a [&'a str],
        scopes: &'a Scopes<'a>,
        names: &'a std::cell::RefCell<HashSet<Name>>,
        index: usize,
        eval: bool,
    ) -> Self {
        let f = &p.functions[index];
        // The script's names are globals.
        let locals: HashMap<&str, u8> = if index == 0 {
            decls[0].iter().enumerate().map(|(i, &n)| (n, i as u8 + 1)).collect()
        } else {
            decls[index][..decls[index].len() - overflow.len()]
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, i as u8 + 1))
                .collect()
        };
        let temp = 1 + locals.len();
        Gen {
            p,
            decls,
            scopes,
            index,
            f,
            asm: Assembler::new(),
            labels: Vec::new(),
            ics: Vec::new(),
            names,
            closures: Vec::new(),
            constants: Vec::new(),
            nregs: 1 + f.params.len(),
            locals,
            overflow,
            shared_ics: HashMap::default(),
            temp,
            acc: Acc::default(),
            reachable: true,
            targets: Vec::new(),
            pending_labels: Vec::new(),
            deferred: Vec::new(),
            handlers: Vec::new(),
            in_try: 0,
            withs: f.withs.clone(),
            eval,
            completion: None,
            cleared: false,
            depth: 0,
            fresh: Vec::new(),
            entering: false,
            trys: Vec::new(),
        }
    }

    fn function(mut self) -> R<FunctionDef> {
        if self.f.params.len() + 1 > LOCAL_REGS {
            return unsupported(self.f.pos, "more parameters than registers");
        }
        // The scope object, with each of its variables (`undefined`, in
        // order: one shape).
        if !self.overflow.is_empty() {
            let env = self.locals[ENV];
            let cap = self.overflow.len().min(crate::vm::MAX_INLINE_CAPACITY as usize) as u8;
            let ic = self.auto_ic(&format!("[]{}", cap));
            self.emit(Op::CreateObject, &[Arg::Count(cap), ic]);
            self.star(env);
            self.emit(Op::LdaUndefined, &[]);
            for &n in self.overflow {
                self.named(Op::SetNamed, env, n);
            }
        }
        if arguments_object(self.p, self.index) {
            let r = self.locals["arguments"];
            self.emit(Op::CreateArguments, &[Arg::Reg(r)]);
        }
        if self.f.rest {
            let k = self.f.params.len() - 1;
            self.emit(Op::CreateRest, &[Arg::Count(k as u8)]);
            self.store_var(&self.f.params[k], self.f.pos)?;
        }
        if self.f.uses_this {
            self.ldar(0);
            self.store_var("%this", self.f.pos)?;
        }
        // (Not if a parameter or variable of the function has the name.)
        if let Some(n) = own_name(self.p, self.index)
            && !self.f.params.iter().any(|p| &**p == n)
            && !explicit_declarations(self.p, self.index).contains(&n)
            && self.locals.contains_key(n)
        {
            self.emit(Op::LdaCallee, &[]);
            self.star(self.locals[n]);
        }
        // (A new frame's: no upvalues of its registers to close yet.)
        self.entering = true;
        self.enter_scope(&self.f.body, self.f.pos)?;
        self.entering = false;
        let mut fns = Vec::new();
        self.f.body.iter().for_each(|s| hoisted(s, &mut fns));
        for i in fns {
            self.closure(i);
            let name = self.p.functions[i].name.as_deref().unwrap();
            self.store_var(name, self.f.pos)?;
        }
        let body = &self.f.body;
        if self.index == 0 {
            // `var x;` makes a global that reads as undefined.
            // Declared before the script runs, `undefined` unless a global
            // of that name exists (`var performance = performance || {}`).
            for name in self.script_vars() {
                self.global(Op::LdaGlobalInsideTypeof, name);
                self.global(Op::StaGlobal, name);
            }
            if self.eval {
                // `eval`'s result is the script's completion value: each
                // expression statement's value, as the spec's completion
                // records have it (see `Gen::completion`).
                let cv = self.alloc(self.f.pos)?;
                self.completion = Some(cv);
                self.emit(Op::LdaUndefined, &[]);
                self.star(cv);
                for s in body {
                    self.stmt(s)?;
                }
                self.ldar(cv);
                self.emit(Op::Return, &[]);
            } else {
                // The script's completion value (printed by `toi`) is its
                // last expression statement's.
                match body.split_last() {
                    Some((
                        Stmt {
                            kind: StmtKind::Expr(e),
                            ..
                        },
                        rest,
                    )) => {
                        for s in rest {
                            self.stmt(s)?;
                        }
                        self.value(e)?;
                        self.emit(Op::Return, &[]);
                    }
                    _ => {
                        for s in body {
                            self.stmt(s)?;
                        }
                    }
                }
            }
        } else {
            for s in body {
                self.stmt(s)?;
            }
        }
        self.emit(Op::LdaUndefined, &[]);
        self.emit(Op::Return, &[]);
        while !self.deferred.is_empty() {
            let (l, s) = self.deferred.remove(0);
            self.bind(l);
            // (A deferred `then` branch is nested in its `if`.)
            self.depth = 1;
            self.stmt(s)?;
            self.depth = 0;
        }
        if self.nregs > 256 {
            return unsupported(self.f.pos, "more than 256 registers");
        }
        let name = function_name(self.p, self.index);
        let upvalues = upvalue_sources(self.p, self.decls, self.scopes, self.index);
        let captures = !self.scopes.captured[self.index].is_empty();
        Ok(FunctionDef {
            name,
            strict: self.f.strict,
            nparams: self.f.params.len() as u32,
            nregs: self.nregs as u32,
            code: self.asm.finish(),
            ics: self.ics,
            closures: self.closures,
            constants: self.constants,
            handlers: self.handlers,
            upvalues,
            captures,
            line: 0,
            col: 0,
            lazy: None,
            kind: function_kind(self.f),
            length: if self.index == 0 { 0 } else { self.f.length as u32 },
        })
    }

    /// The script's `var`s to declare before it runs: those without an
    /// initializer, or whose initializer reads them; not those assigned
    /// first, whose first store then sets the global's field type (an int,
    /// not `undefined`: 53, 80), nor function declarations.
    fn script_vars(&self) -> Vec<&'a str> {
        fn walk<'a>(s: &'a Stmt, init: &mut Vec<&'a str>, bare: &mut Vec<&'a str>, p: &'a Program<'a>) {
            // An initializer reading its own variable (`var p = p || {}`)
            // reads it declared, before it's assigned.
            let mut decl = |ds: &'a [(Name, Option<Expr>)]| {
                for (n, e) in ds {
                    let reads_itself = e.as_ref().is_some_and(|e| {
                        let mut refs = Vec::new();
                        expr_refs(e, &mut refs);
                        refs.contains(&&**n)
                    });
                    if e.is_some() && !reads_itself { &mut *init } else { &mut *bare }.push(n);
                }
            };
            match &s.kind {
                StmtKind::Var(ds) => decl(ds),
                StmtKind::For {
                    init: Some(ForInit::Var(ds)),
                    body,
                    ..
                } => {
                    decl(ds);
                    walk(body, init, bare, p);
                }
                StmtKind::ForIn {
                    var, name, body, ..
                } => {
                    if *var {
                        init.push(name);
                    }
                    walk(body, init, bare, p);
                }
                // (A function declaration's global exists from the start:
                // hoisted, or Annex B's in a block, `undefined` until then.)
                StmtKind::Function(i) => {
                    let f = &p.functions[*i];
                    if f.binding.is_none() || f.annex_b {
                        bare.push(f.name.as_deref().unwrap())
                    }
                }
                StmtKind::If(_, a, b) => {
                    walk(a, init, bare, p);
                    if let Some(b) = b {
                        walk(b, init, bare, p);
                    }
                }
                StmtKind::For { body, .. }
                | StmtKind::While(_, body)
                | StmtKind::DoWhile(body, _)
                | StmtKind::With(_, _, body)
                | StmtKind::Labeled(_, body) => walk(body, init, bare, p),
                StmtKind::Block(b) => b.iter().for_each(|s| walk(s, init, bare, p)),
                StmtKind::Switch(_, cases) => cases
                    .iter()
                    .flat_map(|(_, b)| b)
                    .for_each(|s| walk(s, init, bare, p)),
                StmtKind::Try(b, c, f) => try_blocks(b, c, f).for_each(|s| walk(s, init, bare, p)),
                _ => {}
            }
        }
        let (mut init, mut bare) = (Vec::new(), Vec::new());
        self.f
            .body
            .iter()
            .for_each(|s| walk(s, &mut init, &mut bare, self.p));
        let mut fns = Vec::new();
        self.f.body.iter().for_each(|s| hoisted(s, &mut fns));
        let fns: Vec<&str> = fns
            .iter()
            .map(|&i| self.p.functions[i].name.as_deref().unwrap())
            .collect();
        let mut out: Vec<&str> = Vec::new();
        for n in bare {
            if !init.contains(&n) && !fns.contains(&n) && !out.contains(&n) {
                out.push(n);
            }
        }
        out
    }

    // ---- Emission ----

    fn emit(&mut self, op: Op, args: &[Arg]) {
        if !self.reachable {
            return;
        }
        for a in args {
            if let Arg::Reg(r) = *a {
                self.nregs = self.nregs.max(r as usize + 1);
            }
        }
        // A call's argument window counts as used registers.
        if let [_, Arg::Reg(w), Arg::Count(c), ..] = args[..] {
            let extra = usize::from(matches!(op, Op::Call | Op::CallApplyArguments));
            self.nregs = self.nregs.max(w as usize + c as usize + extra);
        }
        self.asm.emit(op, args);
        match (op, args) {
            (Op::Ldar, &[Arg::Reg(r)]) => {
                self.acc = Acc {
                    regs: RegBits::one(r),
                    smi: None,
                }
            }
            (Op::LdaSmi, &[Arg::Imm(k)]) => {
                self.acc = Acc {
                    regs: RegBits::default(),
                    smi: Some(k),
                }
            }
            (Op::Star, &[Arg::Reg(r)]) => self.acc.regs.insert(r),
            (Op::Mov, &[Arg::Reg(a), Arg::Reg(b)]) => {
                self.acc.regs.remove(b);
                if self.acc.regs.contains(a) {
                    self.acc.regs.insert(b);
                }
            }
            (Op::Jump | Op::Return | Op::Throw, _) => self.reachable = false,
            (
                Op::StaGlobal
                | Op::SetNamed
                | Op::SetKeyed
                | Op::JumpIfTrue
                | Op::JumpIfFalse
                | Op::JumpIfToBooleanTrue
                | Op::JumpIfToBooleanFalse
                | Op::JumpIfLess
                | Op::JumpIfLessSmi
                | Op::JumpIfLessEqual
                | Op::JumpIfGreater
                | Op::JumpIfGreaterEqual
                | Op::JumpIfLessEqualSmi
                | Op::JumpIfGreaterSmi
                | Op::JumpIfGreaterEqualSmi
                | Op::JumpIfStrictEqualSmi,
                _,
            ) => {}
            _ => self.acc = Acc::default(),
        }
    }

    fn ldar(&mut self, r: u8) {
        if !self.acc.regs.contains(r) {
            self.emit(Op::Ldar, &[Arg::Reg(r)]);
        }
    }

    fn star(&mut self, r: u8) {
        if !self.acc.regs.contains(r) {
            // `r` changes: the accumulator holds its new value.
            self.acc.regs.remove(r);
            self.emit(Op::Star, &[Arg::Reg(r)]);
        }
    }

    fn lda_smi(&mut self, k: i32) {
        if self.acc.smi != Some(k) {
            self.emit(Op::LdaSmi, &[Arg::Imm(k)]);
        }
    }

    fn mov(&mut self, from: u8, to: u8) {
        if from == to {
        } else if self.acc.regs.contains(from) {
            self.star(to);
        } else {
            self.emit(Op::Mov, &[Arg::Reg(from), Arg::Reg(to)]);
        }
    }

    /// An inline-cache slot for property `name`: one per instruction, but
    /// past [`SHARED_ICS`] slots (2-byte operands) instructions of the same
    /// `kind` (opcode, and what else their caches depend on) share one.
    fn ic(&mut self, kind: &str, name: &str) -> Arg {
        if self.ics.len() >= SHARED_ICS {
            let key = format!("{kind}\0{name}");
            if let Some(&k) = self.shared_ics.get(&key) {
                return Arg::Index(k);
            }
            self.shared_ics.insert(key, self.ics.len() as u16);
        }
        let name = {
            let mut names = self.names.borrow_mut();
            match names.get(name) {
                Some(n) => n.clone(),
                None => {
                    let n: Name = name.into();
                    names.insert(n.clone());
                    n
                }
            }
        };
        self.ics.push(name);
        Arg::Index(self.ics.len() as u16 - 1)
    }

    /// A slot for an instruction with an implicit inline cache, of `kind`.
    fn auto_ic(&mut self, kind: &str) -> Arg {
        self.ic(kind, "[]")
    }

    fn global(&mut self, op: Op, name: &str) {
        let ic = self.ic(op.name(), name);
        self.emit(op, &[ic]);
    }

    fn named(&mut self, op: Op, obj: u8, name: &str) {
        let ic = self.ic(op.name(), name);
        self.emit(op, &[Arg::Reg(obj), ic]);
    }

    /// `LdaConstant` of `c`, one slot per distinct constant.
    fn constant(&mut self, c: Constant) {
        let i = self.const_index(c);
        self.emit(Op::LdaConstant, &[Arg::Index(i)]);
    }

    /// `LdaConstant` of string `s`.
    fn string(&mut self, s: &str) {
        let i = self.const_str(s);
        self.emit(Op::LdaConstant, &[Arg::Index(i)]);
    }

    /// The index of string constant `s` (made only if new).
    fn const_str(&mut self, s: &str) -> u16 {
        let i = (self.constants.iter())
            .position(|c| matches!(c, Constant::String(x) if x == s))
            .unwrap_or_else(|| {
                self.constants.push(Constant::String(s.to_string()));
                self.constants.len() - 1
            });
        i as u16
    }

    /// The index of constant `c` (equal ones shared).
    fn const_index(&mut self, c: Constant) -> u16 {
        let same = |x: &Constant| match (x, &c) {
            (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        };
        let i = self.constants.iter().position(same).unwrap_or_else(|| {
            self.constants.push(c);
            self.constants.len() - 1
        });
        i as u16
    }

    /// Whether `name` is looked up in `with` objects first: some enclose
    /// the code, and not its block-scoped binding's declaration.
    fn in_with(&self, name: &str) -> bool {
        let skip = self.p.lexicals.get(name).map_or(0, |l| l.withs);
        self.withs.len() > skip
    }

    /// Entering a scope of statements `stmts`: its block-scoped variables
    /// not initialized (each entry a new binding: the upvalues closures
    /// made of their registers closed first), its functions made.
    fn enter_scope(&mut self, stmts: &'a [Stmt<'a>], pos: Pos) -> R<()> {
        for s in stmts {
            if let StmtKind::Lexical(_, ds) = &s.kind {
                for (n, _) in ds {
                    self.fresh_binding(n, true, pos)?;
                }
            }
        }
        for s in stmts {
            if let StmtKind::Function(i) = &s.kind
                && let Some(b) = &self.p.functions[*i].binding
            {
                self.fresh_binding(b, false, pos)?;
                self.closure(*i);
                self.init_var(b, pos)?;
            }
        }
        Ok(())
    }

    /// A new binding for block-scoped `n`: its register's upvalue closed,
    /// and (`hole`) the register not initialized.
    fn fresh_binding(&mut self, n: &str, hole: bool, pos: Pos) -> R<()> {
        match self.resolve(n, pos)? {
            Var::Reg(r) => {
                if !self.entering && self.scopes.captured[self.index].contains(&n) {
                    self.emit(Op::CloseUpvalue, &[Arg::Reg(r)]);
                }
                if hole {
                    self.emit(Op::LdaHole, &[]);
                    self.star(r);
                }
            }
            Var::Scope(_) if hole => {
                self.emit(Op::LdaUndefined, &[]);
                self.init_var(n, pos)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// Initialize variable `n` with the accumulator (no `with` object's).
    fn init_var(&mut self, n: &str, pos: Pos) -> R<()> {
        let withs = std::mem::take(&mut self.withs);
        let r = self.store_raw(n, pos);
        self.withs = withs;
        r
    }

    /// `acc` = intrinsic `name` of registers `first..first + n` (see
    /// `crate::intrinsics`).
    fn intrinsic(&mut self, name: &str, first: u8, n: u8) {
        let id = crate::intrinsics::id(name);
        self.emit(Op::Intrinsic, &[Arg::Count(id), Arg::Reg(first), Arg::Count(n)]);
    }

    fn closure(&mut self, f: usize) {
        let i = self
            .closures
            .iter()
            .position(|&c| c == f)
            .unwrap_or_else(|| {
                self.closures.push(f);
                self.closures.len() - 1
            });
        self.emit(Op::CreateClosure, &[Arg::Index(i as u16)]);
    }

    fn alloc(&mut self, pos: Pos) -> R<u8> {
        if self.temp > 255 {
            return unsupported(pos, "an expression needing more than 256 registers");
        }
        self.temp += 1;
        self.nregs = self.nregs.max(self.temp);
        Ok(self.temp as u8 - 1)
    }

    // ---- Labels and jumps ----

    fn label(&mut self) -> L {
        let label = self.asm.label();
        self.labels.push(LabelInfo {
            label,
            used: false,
            bound: false,
            backward: false,
        });
        L(self.labels.len() - 1)
    }

    /// A label to bind before the jumps to it.
    fn loop_label(&mut self) -> L {
        let l = self.label();
        self.labels[l.0].backward = true;
        l
    }

    fn bind(&mut self, l: L) {
        let info = &mut self.labels[l.0];
        info.bound = true;
        self.asm.bind(info.label);
        if info.used || info.backward {
            self.reachable = true;
            self.acc = Acc::default();
        }
    }

    /// `l` as a jump's operand.
    fn target(&mut self, l: L) -> Arg {
        let info = &mut self.labels[l.0];
        info.used |= self.reachable;
        Arg::Label(info.label)
    }

    fn jump(&mut self, l: L) {
        let t = self.target(l);
        self.emit(Op::Jump, &[t]);
    }

    /// A conditional jump; backwards, as its negation over a `JumpLoop`.
    fn jump_if(&mut self, c: Jmp, l: L) {
        if self.labels[l.0].bound {
            let skip = self.label();
            self.jump_if(c.not(), skip);
            self.jump(l);
            self.bind(skip);
        } else {
            let t = self.target(l);
            self.emit(c.op(), &[t]);
        }
    }

    // ---- try / catch / finally ----

    /// `try block catch (name) body finally f`. The block (and the catch
    /// clause, if there is a `finally`) is covered by handler ranges; an
    /// exception there lands in the handler with its value in `acc`. A
    /// `finally` block runs on every way out: after the statement
    /// completes, in the exception handler (which then throws again), and
    /// copied before each `return`, `break` or `continue` leaving it (see
    /// [`Gen::leave_trys`]).
    fn try_stmt(
        &mut self,
        block: &'a [Stmt],
        catch: &'a Option<(Name, Vec<Stmt>)>,
        finally: &'a Option<Vec<Stmt>>,
        pos: Pos,
    ) -> R<()> {
        let Some(f) = finally else {
            let (name, body) = catch.as_ref().expect("the parser requires `catch` or `finally`");
            return self.try_catch(block, name, body, pos);
        };
        let after = self.label();
        self.open_try(Some(f));
        self.in_try += 1;
        match catch {
            Some((name, body)) => self.try_catch(block, name, body, pos)?,
            None => {
                self.enter_scope(block, pos)?;
                for s in block {
                    self.stmt(s)?;
                }
            }
        }
        self.in_try -= 1;
        let ranges = self.close_try();
        if self.reachable {
            self.enter_scope(f, pos)?;
            for s in f {
                self.stmt(s)?;
            }
            if self.reachable {
                self.jump(after);
            }
        }
        // The exception path: the `finally` block, then throw it again.
        let handler = self.label();
        self.labels[handler.0].used = true;
        self.reachable = false;
        self.bind(handler);
        let at = self.asm.offset() as u32;
        self.handlers.extend(ranges.iter().map(|&(s, e)| (s, e, at)));
        let mark = self.temp;
        let r = self.alloc(pos)?;
        self.star(r);
        self.enter_scope(f, pos)?;
        for s in f {
            self.stmt(s)?;
        }
        if self.reachable {
            self.ldar(r);
            self.emit(Op::Throw, &[]);
        }
        self.temp = mark;
        self.bind(after);
        Ok(())
    }

    /// `try block catch (name) body`: the block, then past the handler,
    /// which puts the exception in the catch parameter's own register.
    fn try_catch(&mut self, block: &'a [Stmt], name: &'a str, body: &'a [Stmt], pos: Pos) -> R<()> {
        self.open_try(None);
        self.in_try += 1;
        self.enter_scope(block, pos)?;
        for s in block {
            self.stmt(s)?;
        }
        self.in_try -= 1;
        let ranges = self.close_try();
        let after = self.label();
        if self.reachable {
            self.jump(after);
        }
        let handler = self.label();
        self.labels[handler.0].used = true;
        self.reachable = false;
        self.bind(handler);
        let at = self.asm.offset() as u32;
        self.handlers.extend(ranges.iter().map(|&(s, e)| (s, e, at)));
        if self.p.lexicals.contains_key(name) {
            // (A block-scoped binding of its own.)
            let mark = self.temp;
            let t = self.alloc(pos)?;
            self.star(t);
            self.fresh_binding(name, false, pos)?;
            self.ldar(t);
            self.init_var(name, pos)?;
            self.temp = mark;
            self.enter_scope(body, pos)?;
            for s in body {
                self.stmt(s)?;
            }
            self.bind(after);
            return Ok(());
        }
        let r = self.alloc(pos)?;
        self.star(r);
        let outer = self.locals.insert(name, r);
        self.enter_scope(body, pos)?;
        for s in body {
            self.stmt(s)?;
        }
        match outer {
            Some(o) => self.locals.insert(name, o),
            None => self.locals.remove(name),
        };
        self.bind(after);
        Ok(())
    }

    fn open_try(&mut self, finally: Option<&'a [Stmt]>) {
        self.trys.push(TryCtx {
            start: self.asm.offset() as u32,
            ranges: Vec::new(),
            finally,
            targets: self.targets.len(),
        });
    }

    /// The innermost `try`'s ranges, closed (empty ones dropped).
    fn close_try(&mut self) -> Vec<(u32, u32)> {
        let mut t = self.trys.pop().expect("a try");
        let end = self.asm.offset() as u32;
        if end > t.start {
            t.ranges.push((t.start, end));
        }
        t.ranges
    }

    /// Before a jump out of the `try` statements from `depth` on: from the
    /// innermost out, close each one's range and copy its `finally` block,
    /// compiled as outside it (still inside the ones further out, whose
    /// handlers cover it).
    fn leave_trys(&mut self, depth: usize) -> R<()> {
        for i in (depth..self.trys.len()).rev() {
            let end = self.asm.offset() as u32;
            let t = &mut self.trys[i];
            if end > t.start {
                t.ranges.push((t.start, end));
            }
            t.start = end;
            if let Some(f) = t.finally {
                let inner = self.trys.split_off(i);
                let r = f.iter().try_for_each(|s| self.stmt(s));
                self.trys.extend(inner);
                r?;
                if !self.reachable {
                    break;
                }
            }
        }
        Ok(())
    }

    /// After that jump: the ranges reopen.
    fn reenter_trys(&mut self, depth: usize) {
        let at = self.asm.offset() as u32;
        for t in &mut self.trys[depth..] {
            t.start = at;
        }
    }

    /// A compare-and-jump (`JumpIfLessSmi k`, `JumpIfGreater r`, …): it
    /// has a loop form, but no negation.
    fn compare_jump(&mut self, op: Op, x: Arg, l: L) {
        let t = self.target(l);
        self.emit(op, &[x, t]);
    }

    // ---- Names ----

    fn resolve(&self, name: &str, pos: Pos) -> R<Var> {
        if let Some(&r) = self.locals.get(name) {
            return Ok(Var::Reg(r));
        }
        if self.overflow.contains(&name) {
            return Ok(Var::Scope(self.locals[ENV]));
        }
        // An enclosing function's variable in its scope object, through
        // that object's register (an upvalue).
        let mut f = self.f.parent;
        while let Some(k) = f.filter(|&k| k != 0) {
            if let Some(at) = self.decls[k].position(name) {
                // (Past the registers: see `generate`.)
                if at >= LOCAL_REGS
                    && let Some(u) = self.scopes.upvalues[self.index].iter().position(|&n| n == ENV)
                {
                    return Ok(Var::ScopeUpvalue(u as u8));
                }
                break;
            }
            f = self.p.functions[k].parent;
        }
        if let Some(k) = self.scopes.upvalues[self.index]
            .iter()
            .position(|&n| n == name)
        {
            return Ok(Var::Upvalue(k as u8));
        }
        let mut f = Some(self.index);
        while let Some(i) = f {
            if i == 0 {
                break;
            }
            let func = &self.p.functions[i];
            if func.binds_name
                && func.name.as_deref() == Some(name)
                && !self.decls[i].contains(&name)
            {
                return unsupported(pos, format!("`{name}`, a function expression's own name"));
            }
            if i != self.index && self.decls[i].contains(&name) {
                return unsupported(
                    pos,
                    format!("`{name}` captured from an enclosing function (closures)"),
                );
            }
            f = self.p.functions[i].parent;
        }
        Ok(if name == "undefined" {
            Var::Undefined
        } else {
            Var::Global
        })
    }

    /// The register of a local variable, or of `this`.
    fn local(&self, e: &Expr) -> Option<u8> {
        match &e.kind {
            ExprKind::This => Some(0),
            // (In a `with`, a name may be the object's.)
            ExprKind::Ident(n) if !self.in_with(n) && !self.needs_check(n, e.pos) => self.locals.get(&**n).copied(),
            _ => None,
        }
    }

    // ---- `with` ----

    /// Inside `with` statements: for each of their objects, innermost
    /// first, `found(r)` where `r` holds the object, reached when the
    /// object has property `name`; then `other()` for the name's own
    /// binding. Every path ends at the returned label, to bind.
    fn with_lookup(
        &mut self,
        name: &str,
        pos: Pos,
        found: &mut dyn FnMut(&mut Self, u8) -> R<()>,
        other: &mut dyn FnMut(&mut Self) -> R<()>,
    ) -> R<L> {
        let end = self.label();
        let withs = std::mem::take(&mut self.withs);
        let skip = self.p.lexicals.get(name).map_or(0, |l| l.withs);
        let mark = self.temp;
        let k = self.alloc(pos)?;
        self.string(original(name));
        self.star(k);
        let r = self.alloc(pos)?;
        for (i, w) in withs.iter().enumerate().rev().filter(|&(i, _)| i >= skip) {
            let next = self.label();
            self.value_inner(&Expr {
                kind: ExprKind::Ident(w.clone()),
                pos,
            })?;
            self.star(r);
            self.emit(Op::TestIn, &[Arg::Reg(k)]);
            self.jump_if(Jmp::False, next);
            // (The lookup continues in the enclosing `with`s only.)
            self.withs = withs[..i].to_vec();
            found(self, r)?;
            self.jump(end);
            self.bind(next);
        }
        self.withs = Vec::new();
        other(self)?;
        self.withs = withs;
        self.temp = mark;
        Ok(end)
    }

    /// `name` inside `with` statements, into the accumulator (`typeof`:
    /// for `typeof name`, where an undeclared global is `undefined`).
    fn with_load(&mut self, name: &str, pos: Pos, type_of: bool) -> R<()> {
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.named(Op::GetNamed, r, original(name));
                Ok(())
            },
            &mut |g| {
                if type_of && matches!(g.resolve(name, pos)?, Var::Global) {
                    g.global(Op::LdaGlobalInsideTypeof, name);
                    return Ok(());
                }
                g.value_inner(&Expr {
                    kind: ExprKind::Ident(name.into()),
                    pos,
                })
            },
        )?;
        self.bind(end);
        Ok(())
    }

    /// Store the accumulator in `name` inside `with` statements (the
    /// accumulator keeps the value).
    fn with_store(&mut self, name: &str, pos: Pos) -> R<()> {
        let mark = self.temp;
        let v = self.alloc(pos)?;
        self.star(v);
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.ldar(v);
                g.named(Op::SetNamed, r, original(name));
                Ok(())
            },
            &mut |g| {
                g.ldar(v);
                g.store_var(name, pos)
            },
        )?;
        self.bind(end);
        self.ldar(v);
        self.temp = mark;
        Ok(())
    }

    /// `name op= v` inside `with` statements: the reference resolved once,
    /// read and written in the object that has `name`.
    fn with_compound(&mut self, name: &str, op: BinOp, v: &Expr, pos: Pos) -> R<()> {
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.named(Op::GetNamed, r, original(name));
                g.apply(op, v, pos)?;
                g.named(Op::SetNamed, r, original(name));
                Ok(())
            },
            &mut |g| {
                g.value_inner(&Expr {
                    kind: ExprKind::Ident(name.into()),
                    pos,
                })?;
                g.apply(op, v, pos)?;
                g.store_var(name, pos)
            },
        )?;
        self.bind(end);
        Ok(())
    }

    /// `delete name` inside `with` statements.
    fn with_delete(&mut self, name: &str, pos: Pos) -> R<()> {
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.string(original(name));
                g.emit(Op::DeleteProperty, &[Arg::Reg(r)]);
                Ok(())
            },
            &mut |g| {
                match g.resolve(name, pos)? {
                    Var::Global => {
                        g.string(name);
                        g.emit(Op::DeleteGlobal, &[]);
                    }
                    _ => g.emit(Op::LdaFalse, &[]),
                }
                Ok(())
            },
        )?;
        self.bind(end);
        Ok(())
    }

    /// `name(args)` inside `with` statements: a method call on the object
    /// that has `name`, else an ordinary call.
    fn with_call(&mut self, name: &str, args: &[Expr], pos: Pos) -> R<()> {
        let n = u8::try_from(args.len()).or_else(|_| unsupported(pos, "more than 255 arguments"))?;
        let c = self.alloc(pos)?;
        let w = self.alloc(pos)?;
        let method = self.alloc(pos)?;
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.named(Op::GetNamed, r, original(name));
                g.star(c);
                g.mov(r, w);
                g.emit(Op::LdaTrue, &[]);
                g.star(method);
                Ok(())
            },
            &mut |g| {
                g.value_inner(&Expr {
                    kind: ExprKind::Ident(name.into()),
                    pos,
                })?;
                g.star(c);
                g.emit(Op::LdaFalse, &[]);
                g.star(method);
                Ok(())
            },
        )?;
        self.bind(end);
        for _ in 0..n {
            self.alloc(pos)?;
        }
        for (i, a) in args.iter().enumerate() {
            self.into(a, w + 1 + i as u8)?;
        }
        let (plain, done) = (self.label(), self.label());
        self.ldar(method);
        self.jump_if(Jmp::False, plain);
        let ic = self.auto_ic(Op::Call.name());
        self.emit(Op::Call, &[Arg::Reg(c), Arg::Reg(w), Arg::Count(n), ic]);
        self.jump(done);
        self.bind(plain);
        let ic = self.auto_ic(Op::CallUndefined.name());
        self.emit(Op::CallUndefined, &[Arg::Reg(c), Arg::Reg(w + 1), Arg::Count(n), ic]);
        self.bind(done);
        Ok(())
    }

    /// Store the accumulator in variable `name`: a `const` throws, a
    /// block-scoped one not initialized yet too.
    fn store_var(&mut self, name: &str, pos: Pos) -> R<()> {
        if self.in_with(name) {
            return self.with_store(name, pos);
        }
        if let Some(l) = self.p.lexicals.get(name) {
            if l.constant {
                if self.needs_check(name, pos) {
                    self.load_var(name, pos)?;
                }
                let mark = self.temp;
                let r = self.alloc(pos)?;
                self.string(&l.original.clone());
                self.star(r);
                self.intrinsic("throw_const", r, 1);
                self.temp = mark;
                return Ok(());
            }
            if self.needs_check(name, pos) {
                let mark = self.temp;
                let v = self.alloc(pos)?;
                self.star(v);
                self.load_var(name, pos)?;
                self.ldar(v);
                self.temp = mark;
            }
        }
        self.store_raw(name, pos)
    }

    /// Initialize (or store, unchecked) variable `name` with the
    /// accumulator.
    fn store_raw(&mut self, name: &str, pos: Pos) -> R<()> {
        if self.in_with(name) {
            return self.with_store(name, pos);
        }
        match self.resolve(name, pos)? {
            Var::Reg(r) => self.star(r),
            Var::Upvalue(k) => {
                self.emit(Op::StaUpvalue, &[Arg::Count(k)]);
            }
            Var::Scope(env) => self.named(Op::SetNamed, env, name),
            Var::ScopeUpvalue(k) => {
                // (`acc` stays the value.)
                let mark = self.temp;
                let v = self.alloc(pos)?;
                self.star(v);
                let env = self.alloc(pos)?;
                self.emit(Op::LdaUpvalue, &[Arg::Count(k)]);
                self.star(env);
                self.ldar(v);
                self.named(Op::SetNamed, env, name);
                self.temp = mark;
            }
            Var::Global | Var::Undefined => self.global(Op::StaGlobal, name),
        }
        Ok(())
    }

    // ---- Expressions ----

    /// `e` into a register: a local variable's own one if nothing in
    /// `later` (evaluated after it, before its use) may assign it, or a new
    /// temporary, which the caller frees.
    fn reg(&mut self, e: &Expr, later: &[&Expr]) -> R<u8> {
        match &e.kind {
            ExprKind::This => return Ok(0),
            ExprKind::Ident(n) => {
                // (A captured one may change in any call, through a closure.)
                if let Some(&r) = self.locals.get(&**n)
                    && !self.in_with(n)
                    && !self.needs_check(n, e.pos)
                    && !later.iter().any(|l| assigns(l, n))
                    && !self.scopes.captured[self.index].contains(&&**n)
                {
                    return Ok(r);
                }
            }
            _ => {}
        }
        self.value(e)?;
        let t = self.alloc(e.pos)?;
        self.star(t);
        Ok(t)
    }

    /// `e` into register `r`.
    fn into(&mut self, e: &Expr, r: u8) -> R<()> {
        match self.local(e) {
            Some(from) => self.mov(from, r),
            None => {
                self.value(e)?;
                self.star(r);
            }
        }
        Ok(())
    }

    /// `e` for its side effects only.
    fn effect(&mut self, e: &Expr) -> R<()> {
        match &e.kind {
            ExprKind::Update(t, d, _) => self.update(t, *d, false, e.pos),
            _ => self.value(e),
        }
    }

    /// `e` into the accumulator.
    fn value(&mut self, e: &Expr) -> R<()> {
        let mark = self.temp;
        self.value_inner(e)?;
        self.temp = mark;
        Ok(())
    }

    fn value_inner(&mut self, e: &Expr) -> R<()> {
        use ExprKind::*;
        match &e.kind {
            Num(_) | Unary(UnOp::Neg, _) if smi(e).is_some() => self.lda_smi(smi(e).unwrap()),
            Num(v) => self.constant(Constant::Number(*v)),
            Unary(UnOp::Neg, x) if matches!(x.kind, Num(_)) => {
                let Num(v) = x.kind else { unreachable!() };
                self.constant(Constant::Number(-v))
            }
            Str(s) => self.string(s),
            Bool(true) => self.emit(Op::LdaTrue, &[]),
            Bool(false) => self.emit(Op::LdaFalse, &[]),
            Null => self.emit(Op::LdaNull, &[]),
            This => self.ldar(0),
            Ident(n) if self.in_with(n) => self.with_load(n, e.pos, false)?,
            Ident(n) => self.load_var(n, e.pos)?,
            Hole | Spread(_) => unreachable!("the parser makes them array elements and arguments only"),
            Template(strings, subs) => {
                let mark = self.temp;
                let first = self.temp as u8;
                for (i, s) in strings.iter().enumerate() {
                    let r = self.alloc(e.pos)?;
                    self.string(s);
                    self.star(r);
                    if let Some(x) = subs.get(i) {
                        let r = self.alloc(e.pos)?;
                        self.into(x, r)?;
                    }
                }
                let n = (self.temp - mark) as u8;
                self.intrinsic("template", first, n);
                self.temp = mark;
            }
            _ => self.value_rest(e)?,
        }
        Ok(())
    }

    /// Variable `name` into the accumulator (checked if block-scoped and
    /// maybe not initialized: see [`Gen::needs_check`]).
    fn load_var(&mut self, n: &str, pos: Pos) -> R<()> {
        self.load_raw(n, pos)?;
        if self.needs_check(n, pos) {
            let k = self.const_str(&self.p.lexicals[n].original.clone());
            self.emit(Op::CheckHole, &[Arg::Index(k)]);
        }
        Ok(())
    }

    /// Whether a reference to `name` at `pos` may find it not initialized:
    /// a block-scoped variable referred to before its declaration's end, or
    /// from a function that may run before it (a declaration, or one made
    /// before it), or in a `switch` (which may jump past it).
    fn needs_check(&self, name: &str, pos: Pos) -> bool {
        let Some(l) = self.p.lexicals.get(name) else {
            return false;
        };
        if l.hoisted {
            return false;
        }
        if l.switch {
            return true;
        }
        if self.index == l.func {
            return pos < l.end;
        }
        self.f.declaration || self.f.pos < l.end
    }

    fn load_raw(&mut self, n: &str, pos: Pos) -> R<()> {
        let e = &Expr { kind: ExprKind::Null, pos };
        match self.resolve(n, e.pos)? {
                Var::Reg(r) => self.ldar(r),
                Var::Upvalue(k) => {
                    self.emit(Op::LdaUpvalue, &[Arg::Count(k)]);
                }
                Var::Scope(env) => self.named(Op::GetNamed, env, n),
                Var::ScopeUpvalue(k) => {
                    let mark = self.temp;
                    let env = self.alloc(e.pos)?;
                    self.emit(Op::LdaUpvalue, &[Arg::Count(k)]);
                    self.star(env);
                    self.named(Op::GetNamed, env, n);
                    self.temp = mark;
                }
                Var::Undefined => self.emit(Op::LdaUndefined, &[]),
                Var::Global => self.global(Op::LdaGlobal, n),
            }
        Ok(())
    }

    fn value_rest(&mut self, e: &Expr) -> R<()> {
        use ExprKind::*;
        match &e.kind {
            // An array with room for its elements, each appended. (Stored
            // by index, Splay's `[0, 1, …, 9]` went through the runtime at
            // the first and grew twice: 17% of its time.)
            Array(elems) => {
                let n = elems.len().min(u8::MAX as usize) as u8;
                self.emit(Op::CreateArray, &[Arg::Count(n)]);
                if !elems.is_empty() {
                    let a = self.alloc(e.pos)?;
                    self.star(a);
                    for el in elems {
                        match &el.kind {
                            Hole => self.intrinsic("hole", a, 1),
                            Spread(x) => {
                                let mark = self.temp;
                                let t = self.alloc(e.pos)?;
                                self.into(x, t)?;
                                self.intrinsic("spread", a, 2);
                                self.temp = mark;
                            }
                            _ => {
                                self.value(el)?;
                                self.emit(Op::PushLiteral, &[Arg::Reg(a)]);
                            }
                        }
                    }
                    self.ldar(a);
                }
            }
            TemplateObject(parts) => {
                static SITES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
                let site = SITES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let mark = self.temp;
                let first = self.alloc(e.pos)?;
                self.constant(Constant::Number(site as f64));
                self.star(first);
                for (cooked, raw) in parts {
                    let r = self.alloc(e.pos)?;
                    match cooked {
                        Some(c) => self.string(c),
                        None => self.emit(Op::LdaUndefined, &[]),
                    }
                    self.star(r);
                    let r = self.alloc(e.pos)?;
                    self.string(raw);
                    self.star(r);
                }
                let n = (self.temp - mark) as u8;
                self.intrinsic("template_object", first, n);
                self.temp = mark;
            }
            Intrinsic(name, args) => {
                let mark = self.temp;
                let first = self.temp as u8;
                for a in args {
                    let r = self.alloc(e.pos)?;
                    self.into(a, r)?;
                }
                self.intrinsic(name, first, args.len() as u8);
                self.temp = mark;
            }
            // A plain object with room for the properties, then each one
            // stored in order (a later duplicate wins).
            Object(props) => {
                // (`__proto__: v` sets the prototype: the store reaches
                // `Object.prototype`'s accessor.)
                // An index key is an element (`o[1]` reads it), stored as
                // `o[k] = v`.
                let index = |k: &str| {
                    k.parse::<u32>()
                        .ok()
                        .filter(|&i| i != u32::MAX && i.to_string() == k)
                };
                let named = props
                    .iter()
                    .filter(|(k, _)| matches!(k, PropKey::Name(k) if index(k).is_none()))
                    .count();
                let cap = named.min(crate::vm::MAX_INLINE_CAPACITY as usize) as u8;
                let ic = self.auto_ic(&format!("[]{}", cap));
                self.emit(Op::CreateObject, &[Arg::Count(cap), ic]);
                if !props.is_empty() {
                    let mark = self.temp;
                    let o = self.alloc(e.pos)?;
                    self.star(o);
                    for (key, v) in props {
                        let name = match key {
                            PropKey::Name(n) => n,
                            PropKey::Computed(k) => {
                                // `[o, k, v]` for the runtime.
                                let mark = self.temp;
                                let t = self.alloc(e.pos)?;
                                self.mov(o, t);
                                let kr = self.alloc(e.pos)?;
                                self.into(k, kr)?;
                                let vr = self.alloc(e.pos)?;
                                let which = match v.kind {
                                    Accessor(set, f) => {
                                        self.closure(f);
                                        if set { "define_setter" } else { "define_getter" }
                                    }
                                    _ => {
                                        self.value(v)?;
                                        "define_field"
                                    }
                                };
                                self.star(vr);
                                self.intrinsic(which, t, 3);
                                self.temp = mark;
                                continue;
                            }
                        };
                        if let Accessor(set, f) = v.kind {
                            self.closure(f);
                            let k = self.const_str(name);
                            let kind = u8::from(set);
                            self.emit(Op::DefineAccessor, &[Arg::Reg(o), Arg::Index(k), Arg::Count(kind)]);
                            continue;
                        }
                        match index(name) {
                            Some(i) => {
                                let k = self.alloc(e.pos)?;
                                match i32::try_from(i) {
                                    Ok(i) => self.emit(Op::LdaSmi, &[Arg::Imm(i)]),
                                    Err(_) => self.constant(Constant::Number(i as f64)),
                                }
                                self.star(k);
                                self.value(v)?;
                                let ic = self.auto_ic("[]keyed");
                                self.emit(Op::SetKeyed, &[Arg::Reg(o), Arg::Reg(k), ic]);
                                self.temp -= 1;
                            }
                            None => {
                                self.value(v)?;
                                self.named(Op::SetNamed, o, name);
                            }
                        }
                    }
                    self.ldar(o);
                    self.temp = mark;
                }
            }
            Function(i) => self.closure(*i),
            Accessor(..) => unreachable!("the parser makes them object literal values only"),
            RegExp(p, f) => {
                let p = self.const_str(p);
                let f = self.const_str(f);
                self.emit(Op::CreateRegExp, &[Arg::Index(p), Arg::Index(f)]);
            }
            Member(o, name) => {
                let r = self.reg(o, &[])?;
                self.named(Op::GetNamed, r, name);
            }
            Index(o, k) => {
                let r = self.reg(o, &[k])?;
                self.value(k)?;
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::GetKeyed, &[Arg::Reg(r), ic]);
            }
            Call(f, args) => self.call(f, args, false, e.pos)?,
            New(f, args) => self.call(f, args, true, e.pos)?,
            Unary(UnOp::Not, _) => self.boolean(e)?,
            Unary(UnOp::Void, x) => {
                self.effect(x)?;
                self.emit(Op::LdaUndefined, &[]);
            }
            // `delete`: a property or element (the key in `acc`), a global
            // (its name in `acc`); a variable isn't deleted (false), and
            // anything else is just evaluated (true).
            Unary(UnOp::Delete, x) => match &x.kind {
                Member(o, name) => {
                    let mark = self.temp;
                    let r = self.reg(o, &[])?;
                    self.string(name);
                    self.emit(Op::DeleteProperty, &[Arg::Reg(r)]);
                    self.temp = mark;
                }
                Index(o, k) => {
                    let mark = self.temp;
                    let r = self.reg(o, &[k])?;
                    self.value(k)?;
                    self.emit(Op::DeleteProperty, &[Arg::Reg(r)]);
                    self.temp = mark;
                }
                Ident(n) if self.in_with(n) => self.with_delete(n, x.pos)?,
                Ident(n) => match self.resolve(n, x.pos)? {
                    Var::Global => {
                        self.string(n);
                        self.emit(Op::DeleteGlobal, &[]);
                    }
                    _ => self.emit(Op::LdaFalse, &[]),
                },
                _ => {
                    self.effect(x)?;
                    self.emit(Op::LdaTrue, &[]);
                }
            },
            Unary(UnOp::Typeof, x) => {
                // An undeclared global is "undefined", not a ReferenceError.
                match &x.kind {
                    Ident(n) if self.in_with(n) => self.with_load(n, x.pos, true)?,
                    Ident(n) if matches!(self.resolve(n, x.pos)?, Var::Global) => {
                        self.global(Op::LdaGlobalInsideTypeof, n)
                    }
                    _ => self.value(x)?,
                }
                self.emit(Op::TypeOf, &[]);
            }
            Unary(op @ (UnOp::Plus | UnOp::Neg | UnOp::BitNot), x) => {
                self.value(x)?;
                let op = match op {
                    UnOp::Plus => Op::ToNumber,
                    UnOp::Neg => Op::Negate,
                    _ => Op::BitNot,
                };
                self.emit(op, &[]);
            }
            Update(t, d, prefix) => self.update(t, *d, !*prefix, e.pos)?,
            Binary(op, l, r) => self.binary(*op, l, r, e.pos)?,
            Logical(and, l, r) => {
                let end = self.label();
                self.value(l)?;
                let c = match (and, is_boolean(l)) {
                    (true, true) => Jmp::False,
                    (true, false) => Jmp::ToBooleanFalse,
                    (false, true) => Jmp::True,
                    (false, false) => Jmp::ToBooleanTrue,
                };
                self.jump_if(c, end);
                self.value(r)?;
                self.bind(end);
            }
            Cond(c, a, b) => {
                let (lt, lf, end) = (self.label(), self.label(), self.label());
                self.test(c, lt, lf, Fall::True)?;
                self.bind(lt);
                self.value(a)?;
                self.jump(end);
                self.bind(lf);
                self.value(b)?;
                self.bind(end);
            }
            Assign(op, t, v) => self.assign(*op, t, v, e.pos)?,
            Comma(a, b) => {
                self.effect(a)?;
                self.value(b)?;
            }
            _ => unreachable!("value_inner's"),
        }
        Ok(())
    }

    /// A boolean-valued `e` the instruction set has no single opcode for,
    /// by branching.
    fn boolean(&mut self, e: &Expr) -> R<()> {
        let (lt, lf, end) = (self.label(), self.label(), self.label());
        self.test(e, lt, lf, Fall::True)?;
        self.bind(lt);
        self.emit(Op::LdaTrue, &[]);
        self.jump(end);
        self.bind(lf);
        self.emit(Op::LdaFalse, &[]);
        self.bind(end);
        Ok(())
    }

    /// The `…Smi` form of `op` with a literal right operand `k`, as the
    /// opcode and its immediate.
    fn smi_form(op: BinOp, k: i32) -> Option<(Op, i32)> {
        Some(match op {
            BinOp::Add => (Op::AddSmi, k),
            BinOp::Sub => (Op::SubSmi, k),
            BinOp::Mul => (Op::MulSmi, k),
            BinOp::Div => (Op::DivSmi, k),
            BinOp::Shl => (Op::ShiftLeftSmi, k),
            BinOp::Sar => (Op::ShiftRightSmi, k),
            BinOp::Shr => (Op::ShiftRightLogicalSmi, k),
            BinOp::Mod => (Op::ModSmi, k),
            BinOp::BitAnd => (Op::BitAndSmi, k),
            BinOp::BitOr => (Op::BitOrSmi, k),
            BinOp::Eq => (Op::TestEqualSmi, k),
            _ => return None,
        })
    }

    /// For `x === undefined`, `null === x`, `x == null`, … (`op` `==` or
    /// `===`): `x` and the test of it.
    fn nullish_test<'e, 'x>(op: BinOp, l: &'e Expr<'x>, r: &'e Expr<'x>) -> Option<(&'e Expr<'x>, Op)> {
        let constant = |e: &Expr| match &e.kind {
            ExprKind::Null => Some(Op::TestNull),
            // `undefined` names the global (it can't be reassigned, but a
            // local may shadow it).
            ExprKind::Ident(n) if &**n == "undefined" => Some(Op::TestUndefined),
            _ => None,
        };
        let (x, c) = match (constant(l), constant(r)) {
            (_, Some(c)) => (l, c),
            (Some(c), None) => (r, c),
            _ => return None,
        };
        match op {
            BinOp::StrictEq => Some((x, c)),
            BinOp::Eq => Some((x, Op::TestNullish)),
            _ => None,
        }
    }

    /// The register form of `op`: `r OP acc`.
    fn reg_form(op: BinOp) -> Option<Op> {
        Some(match op {
            BinOp::Add => Op::Add,
            BinOp::Sub => Op::Sub,
            BinOp::Mul => Op::Mul,
            BinOp::Div => Op::Div,
            BinOp::Lt => Op::TestLessThan,
            BinOp::Gt => Op::TestGreaterThan,
            BinOp::Le => Op::TestLessThanOrEqual,
            BinOp::Ge => Op::TestGreaterThanOrEqual,
            BinOp::Eq => Op::TestEqual,
            BinOp::StrictEq => Op::TestStrictEqual,
            BinOp::Mod => Op::Mod,
            BinOp::BitAnd => Op::BitAnd,
            BinOp::BitOr => Op::BitOr,
            BinOp::BitXor => Op::BitXor,
            BinOp::Shl => Op::ShiftLeft,
            BinOp::Sar => Op::ShiftRight,
            BinOp::Shr => Op::ShiftRightLogical,
            BinOp::Instanceof => Op::TestInstanceOf,
            BinOp::In => Op::TestIn,
            _ => return None,
        })
    }

    fn binary(&mut self, op: BinOp, l: &Expr, r: &Expr, pos: Pos) -> R<()> {
        if let Some((sop, k)) = smi(r).and_then(|k| Self::smi_form(op, k)) {
            self.value(l)?;
            self.emit(sop, &[Arg::Imm(k)]);
            return Ok(());
        }
        // `k * x` is `x * k`: multiplication of numbers commutes, and a
        // literal has no side effects to order.
        if op == BinOp::Mul
            && let Some(k) = smi(l)
        {
            self.value(r)?;
            self.emit(Op::MulSmi, &[Arg::Imm(k)]);
            return Ok(());
        }
        // `x === undefined`, `x == null`, …: a test of one value.
        if let Some((x, test)) = Self::nullish_test(op, l, r) {
            self.value(x)?;
            self.emit(test, &[]);
            return Ok(());
        }
        if matches!(op, BinOp::Ne | BinOp::StrictNe) {
            // `l != r` has no opcode: branch on `l == r`.
            let (lt, lf, end) = (self.label(), self.label(), self.label());
            self.test_binary(op, l, r, lt, lf, Fall::True, pos)?;
            self.bind(lt);
            self.emit(Op::LdaTrue, &[]);
            self.jump(end);
            self.bind(lf);
            self.emit(Op::LdaFalse, &[]);
            self.bind(end);
            return Ok(());
        }
        let Some(rop) = Self::reg_form(op) else {
            return unsupported(pos, format!("the operator `{}`", op.symbol()));
        };
        let lr = self.reg(l, &[r])?;
        self.value(r)?;
        self.emit(rop, &[Arg::Reg(lr)]);
        Ok(())
    }

    /// The accumulator `OP` `v`, where the accumulator holds the left
    /// operand (of a compound assignment).
    fn apply(&mut self, op: BinOp, v: &Expr, pos: Pos) -> R<()> {
        if let Some((sop, k)) = smi(v).and_then(|k| Self::smi_form(op, k)) {
            self.emit(sop, &[Arg::Imm(k)]);
            return Ok(());
        }
        let Some(rop) = Self::reg_form(op) else {
            return unsupported(pos, format!("the operator `{}=`", op.symbol()));
        };
        let t = self.alloc(pos)?;
        self.star(t);
        self.value(v)?;
        self.emit(rop, &[Arg::Reg(t)]);
        Ok(())
    }

    fn assign(&mut self, op: Option<BinOp>, t: &Expr, v: &Expr, pos: Pos) -> R<()> {
        match &t.kind {
            ExprKind::Ident(n) if op.is_some() && self.in_with(n) => {
                self.with_compound(n, op.unwrap(), v, pos)
            }
            ExprKind::Ident(n) => {
                match (op, self.resolve(n, t.pos)?) {
                    (None, _) => self.value(v)?,
                    // `x op= v` with `x` in a register: `v`, then `Op r_x`.
                    (Some(op), Var::Reg(r))
                        if smi(v).and_then(|k| Self::smi_form(op, k)).is_none()
                            && !self.needs_check(n, t.pos)
                            && Self::reg_form(op).is_some()
                            && !assigns(v, n) =>
                    {
                        self.value(v)?;
                        self.emit(Self::reg_form(op).unwrap(), &[Arg::Reg(r)]);
                    }
                    (Some(op), _) => {
                        self.value(t)?;
                        self.apply(op, v, pos)?;
                    }
                }
                self.store_var(n, t.pos)
            }
            ExprKind::Member(o, name) => {
                let ro = self.reg(o, &[v])?;
                match op {
                    None => self.value(v)?,
                    Some(op) => {
                        self.named(Op::GetNamed, ro, name);
                        self.apply(op, v, pos)?;
                    }
                }
                self.named(Op::SetNamed, ro, name);
                Ok(())
            }
            ExprKind::Index(o, k) => {
                let ro = self.reg(o, &[k, v])?;
                let rk = self.reg(k, &[v])?;
                match op {
                    None => self.value(v)?,
                    Some(op) => {
                        self.ldar(rk);
                        let ic = self.auto_ic("[]keyed");
                        self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                        self.apply(op, v, pos)?;
                    }
                }
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
                Ok(())
            }
            _ => unreachable!("the parser checks assignment targets"),
        }
    }

    /// `++t` or `--t` (`d` = ±1), leaving the new value in the accumulator.
    /// `++t` / `--t`; with `postfix`, `t++` / `t--` whose value is used: the
    /// old value, as a number, ends in `acc`.
    fn update(&mut self, t: &Expr, d: i8, postfix: bool, pos: Pos) -> R<()> {
        let mark = self.temp;
        let old = if postfix {
            Some(self.alloc(pos)?)
        } else {
            None
        };
        let step = |g: &mut Self| {
            if let Some(o) = old {
                g.emit(Op::ToNumber, &[]);
                g.star(o);
            }
            if d > 0 {
                g.emit(Op::Inc, &[]);
            } else {
                g.emit(Op::SubSmi, &[Arg::Imm(1)]);
            }
        };
        match &t.kind {
            ExprKind::Ident(n) if self.in_with(n) => {
                // (The reference is resolved once: `name` in the object
                // that has it, read and written there.)
                let end = self.with_lookup(
                    n,
                    pos,
                    &mut |g, r| {
                        g.named(Op::GetNamed, r, original(n));
                        step(g);
                        g.named(Op::SetNamed, r, original(n));
                        Ok(())
                    },
                    &mut |g| {
                        g.value_inner(&Expr {
                            kind: ExprKind::Ident(n.clone()),
                            pos,
                        })?;
                        step(g);
                        g.store_var(n, pos)
                    },
                )?;
                self.bind(end);
            }
            ExprKind::Ident(n) => {
                self.value(t)?;
                step(self);
                self.store_var(n, pos)?;
            }
            ExprKind::Member(o, name) => {
                let ro = self.reg(o, &[])?;
                self.named(Op::GetNamed, ro, name);
                step(self);
                self.named(Op::SetNamed, ro, name);
            }
            ExprKind::Index(o, k) => {
                let ro = self.reg(o, &[k])?;
                let rk = self.reg(k, &[])?;
                self.ldar(rk);
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                step(self);
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
            }
            _ => unreachable!("the parser checks increment targets"),
        }
        if let Some(o) = old {
            self.ldar(o);
        }
        self.temp = mark;
        Ok(())
    }

    /// A call, or with `new`, a construction. The callee goes in a
    /// temporary, the receiver (for a method call) and the arguments in
    /// consecutive ones after it; a single argument already in a register
    /// is passed in place.
    fn call(&mut self, f: &Expr, args: &[Expr], new: bool, pos: Pos) -> R<()> {
        if args.iter().any(|a| matches!(a.kind, ExprKind::Spread(_))) {
            return self.call_spread(f, args, new, pos);
        }
        if let ExprKind::Ident(n) = &f.kind
            && !new
            && self.in_with(n)
        {
            return self.with_call(n, args, pos);
        }
        let n =
            u8::try_from(args.len()).or_else(|_| unsupported(pos, "more than 255 arguments"))?;
        let method = !new && matches!(f.kind, ExprKind::Member(..) | ExprKind::Index(..));
        // `g.apply(x, arguments)` passing the frame's arguments on.
        let forward = method && apply_arguments(f, args) && forwards_arguments(self.p, self.index);
        let c = self.alloc(pos)?;
        let w;
        if method {
            w = self.alloc(pos)?;
            let (o, key) = match &f.kind {
                ExprKind::Member(o, name) => (o, Ok(name)),
                ExprKind::Index(o, k) => (o, Err(k)),
                _ => unreachable!(),
            };
            let ro = match self.local(o) {
                Some(r) => r,
                None => {
                    self.value(o)?;
                    self.star(w);
                    w
                }
            };
            match key {
                Ok(name) => self.named(Op::GetNamed, ro, name),
                Err(k) => {
                    self.value(k)?;
                    let ic = self.auto_ic("[]keyed");
                    self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                }
            }
            self.star(c);
            self.mov(ro, w);
            for _ in 0..n {
                self.alloc(pos)?;
            }
            // (Not `arguments`, which `CallApplyArguments` passes.)
            for (i, a) in args.iter().enumerate().take(if forward { 1 } else { args.len() }) {
                self.into(a, w + 1 + i as u8)?;
            }
        } else {
            self.value(f)?;
            self.star(c);
            match (args, args.first().and_then(|a| self.local(a))) {
                ([_], Some(r)) => w = r,
                _ => {
                    w = self.temp as u8;
                    for _ in 0..n {
                        self.alloc(pos)?;
                    }
                    for (i, a) in args.iter().enumerate() {
                        self.into(a, w + i as u8)?;
                    }
                }
            }
        }
        let op = match (new, method) {
            _ if forward => Op::CallApplyArguments,
            (true, _) => Op::Construct,
            (false, true) => Op::Call,
            (false, false) => Op::CallUndefined,
        };
        let ic = self.auto_ic(op.name());
        self.emit(op, &[Arg::Reg(c), Arg::Reg(w), Arg::Count(n), ic]);
        Ok(())
    }

    /// A call or `new` with a spread argument: the arguments into an
    /// array, for the runtime (`[f, this, args]` or `[f, args]`).
    fn call_spread(&mut self, f: &Expr, args: &[Expr], new: bool, pos: Pos) -> R<()> {
        let mark = self.temp;
        let c = self.alloc(pos)?;
        let w = if new { c } else { self.alloc(pos)? };
        match &f.kind {
            ExprKind::Member(o, name) if !new => {
                self.into(o, w)?;
                self.named(Op::GetNamed, w, name);
                self.star(c);
            }
            ExprKind::Index(o, k) if !new => {
                self.into(o, w)?;
                self.value(k)?;
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::GetKeyed, &[Arg::Reg(w), ic]);
                self.star(c);
            }
            _ => {
                self.into(f, c)?;
                if !new {
                    self.emit(Op::LdaUndefined, &[]);
                    self.star(w);
                }
            }
        }
        let a = self.alloc(pos)?;
        self.emit(Op::CreateArray, &[Arg::Count(args.len().min(255) as u8)]);
        self.star(a);
        for x in args {
            match &x.kind {
                ExprKind::Spread(x) => {
                    let m = self.temp;
                    let t = self.alloc(pos)?;
                    self.into(x, t)?;
                    self.intrinsic("spread", a, 2);
                    self.temp = m;
                }
                _ => {
                    self.value(x)?;
                    self.emit(Op::PushLiteral, &[Arg::Reg(a)]);
                }
            }
        }
        if new {
            self.intrinsic("construct_spread", c, 2);
        } else {
            self.intrinsic("call_spread", c, 3);
        }
        self.temp = mark;
        Ok(())
    }

    // ---- Tests ----

    /// Jump to `t` if `e` is truthy and to `f` otherwise, one of which is
    /// reached by falling through (see [`Fall`]).
    fn test(&mut self, e: &Expr, t: L, f: L, fall: Fall) -> R<()> {
        let mark = self.temp;
        match &e.kind {
            ExprKind::Unary(UnOp::Not, x) => self.test(x, f, t, fall.flip())?,
            ExprKind::Logical(true, a, b) => {
                let mid = self.label();
                self.test(a, mid, f, Fall::True)?;
                self.bind(mid);
                self.test(b, t, f, fall)?;
            }
            ExprKind::Logical(false, a, b) => {
                let mid = self.label();
                self.test(a, t, mid, Fall::False)?;
                self.bind(mid);
                self.test(b, t, f, fall)?;
            }
            ExprKind::Bool(v) => {
                if *v && fall == Fall::False {
                    self.jump(t);
                } else if !*v && fall == Fall::True {
                    self.jump(f);
                }
            }
            ExprKind::Binary(op, l, r) => self.test_binary(*op, l, r, t, f, fall, e.pos)?,
            _ => {
                self.value(e)?;
                self.branch(is_boolean(e), t, f, fall);
            }
        }
        self.temp = mark;
        Ok(())
    }

    /// Jump on the value in the accumulator.
    fn branch(&mut self, boolean: bool, t: L, f: L, fall: Fall) {
        match (fall, boolean) {
            (Fall::True, true) => self.jump_if(Jmp::False, f),
            (Fall::True, false) => self.jump_if(Jmp::ToBooleanFalse, f),
            (Fall::False, true) => self.jump_if(Jmp::True, t),
            (Fall::False, false) => self.jump_if(Jmp::ToBooleanTrue, t),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn test_binary(
        &mut self,
        op: BinOp,
        l: &Expr,
        r: &Expr,
        t: L,
        f: L,
        fall: Fall,
        pos: Pos,
    ) -> R<()> {
        match op {
            BinOp::Ne => return self.test_binary(BinOp::Eq, l, r, f, t, fall.flip(), pos),
            BinOp::StrictNe => {
                return self.test_binary(BinOp::StrictEq, l, r, f, t, fall.flip(), pos);
            }
            // The relational compare-and-jumps only jump when true (a
            // negation isn't one: NaN makes both false).
            BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge if fall == Fall::False => {
                // A local `r` is read at the jump, after `l`: what `l OP r`
                // reads anyway, even if `l` assigns it.
                if let Some(k) = smi(r) {
                    self.value(l)?;
                    self.compare_jump(Self::jump_smi_op(op), Arg::Imm(k), t);
                    return Ok(());
                }
                if let Some(rr) = self.local(r) {
                    self.value(l)?;
                    self.compare_jump(Self::jump_reg_op(op), Arg::Reg(rr), t);
                    return Ok(());
                }
            }
            _ => {}
        }
        let tests_one_value = Self::nullish_test(op, l, r).is_some();
        self.binary(op, l, r, pos)?;
        let boolean = tests_one_value
            || matches!(
                op,
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge | BinOp::Eq | BinOp::StrictEq
            );
        self.branch(boolean, t, f, fall);
        Ok(())
    }

    /// Whether a test of `e` jumps more cheaply when true than when false.
    fn jumps_when_true(&self, e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Binary(BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge, _, r)
            if smi(r).is_some() || self.local(r).is_some())
    }

    /// The compare-and-jump `acc OP imm` for relational `op`.
    fn jump_smi_op(op: BinOp) -> Op {
        match op {
            BinOp::Lt => Op::JumpIfLessSmi,
            BinOp::Le => Op::JumpIfLessEqualSmi,
            BinOp::Gt => Op::JumpIfGreaterSmi,
            _ => Op::JumpIfGreaterEqualSmi,
        }
    }

    /// The compare-and-jump `acc OP r` for relational `op`.
    fn jump_reg_op(op: BinOp) -> Op {
        match op {
            BinOp::Lt => Op::JumpIfLess,
            BinOp::Le => Op::JumpIfLessEqual,
            BinOp::Gt => Op::JumpIfGreater,
            _ => Op::JumpIfGreaterEqual,
        }
    }

    // ---- Statements ----

    fn stmt(&mut self, s: &'a Stmt) -> R<()> {
        self.depth += 1;
        let r = self.stmt_inner(s);
        self.depth -= 1;
        r
    }

    fn stmt_inner(&mut self, s: &'a Stmt) -> R<()> {
        let mark = self.temp;
        let labels = std::mem::take(&mut self.pending_labels);
        match &s.kind {
            StmtKind::Var(ds) => self.var_decls(ds)?,
            StmtKind::Lexical(_, ds) => self.lexical_decls(ds)?,
            // A function declaration in a block: its binding there was
            // made where the block starts; in sloppy mode its `var` (Annex
            // B), `undefined` so far, gets it here.
            StmtKind::Function(i) if self.p.functions[*i].binding.is_some() => {
                let f = &self.p.functions[*i];
                if f.annex_b {
                    let b = f.binding.as_deref().unwrap();
                    self.load_raw(b, s.pos)?;
                    self.init_var(f.name.as_deref().unwrap(), s.pos)?;
                }
            }
            StmtKind::Function(i) if self.depth > 1 => {
                self.closure(*i);
                let name = self.p.functions[*i].name.as_deref().unwrap();
                self.store_var(name, s.pos)?;
            }
            StmtKind::Function(_) | StmtKind::Empty => {}
            StmtKind::Expr(e) if self.completion.is_some() => {
                self.value(e)?;
                self.star(self.completion.unwrap());
            }
            StmtKind::Expr(e) => self.effect(e)?,
            // A statement with a body completes as `undefined` unless an
            // expression statement in it says otherwise (UpdateEmpty).
            StmtKind::If(..)
            | StmtKind::For { .. }
            | StmtKind::ForIn { .. }
            | StmtKind::While(..)
            | StmtKind::DoWhile(..)
            | StmtKind::Switch(..)
            | StmtKind::Try(..)
            | StmtKind::With(..)
                if self.completion.is_some() && !self.cleared =>
            {
                self.emit(Op::LdaUndefined, &[]);
                self.star(self.completion.unwrap());
                self.cleared = true;
                let r = self.stmt_inner(s);
                self.cleared = false;
                return r;
            }
            StmtKind::If(c, then, None)
                if self.in_try == 0 && returns(then) && self.jumps_when_true(c) =>
            {
                // The `then` branch goes out of line, so the test can jump
                // to it.
                let (lt, cont) = (self.label(), self.label());
                self.test(c, lt, cont, Fall::False)?;
                self.bind(cont);
                self.deferred.push((lt, then));
            }
            StmtKind::If(c, then, other) => {
                let (lt, lf) = (self.label(), self.label());
                self.test(c, lt, lf, Fall::True)?;
                self.bind(lt);
                self.stmt(then)?;
                match other {
                    None => self.bind(lf),
                    Some(other) => {
                        let end = self.label();
                        self.jump(end);
                        self.bind(lf);
                        self.stmt(other)?;
                        self.bind(end);
                    }
                }
            }
            StmtKind::For {
                init,
                test,
                update,
                body,
            } => {
                let mut fresh = Vec::new();
                match init {
                    Some(ForInit::Var(ds)) => self.var_decls(ds)?,
                    Some(ForInit::Lexical(_, ds)) => {
                        for (n, _) in ds {
                            self.fresh_binding(n, true, s.pos)?;
                            if self.scopes.captured[self.index].contains(&&**n) {
                                fresh.push(&**n);
                            }
                        }
                        self.lexical_decls(ds)?;
                    }
                    Some(ForInit::Expr(e)) => self.effect(e)?,
                    None => {}
                }
                let first_passes = match test {
                    None => true,
                    Some(c) => first_test_passes(init.as_ref(), c),
                };
                self.fresh = fresh;
                self.looping(labels, test.as_ref(), update.as_ref(), body, !first_passes)?;
            }
            StmtKind::While(c, body) => {
                let first_passes = matches!(c.kind, ExprKind::Bool(true));
                self.looping(labels, Some(c), None, body, !first_passes)?;
            }
            StmtKind::DoWhile(body, c) => self.looping(labels, Some(c), None, body, false)?,
            StmtKind::With(o, name, body) => {
                self.value(o)?;
                let saved = std::mem::take(&mut self.withs);
                self.store_var(name, s.pos)?;
                self.withs = saved;
                self.withs.push(name.clone());
                let res = self.stmt(body);
                self.withs.pop();
                res?;
            }
            StmtKind::ForIn {
                name, object, body, fresh, ..
            } => self.for_in(labels, name, *fresh, object, body, s.pos)?,
            StmtKind::Switch(d, cases) => self.switch(labels, d, cases)?,
            StmtKind::Try(block, catch, finally) => self.try_stmt(block, catch, finally, s.pos)?,
            StmtKind::Throw(e) => {
                self.value(e)?;
                self.emit(Op::Throw, &[]);
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) => self.value(e)?,
                    None => self.emit(Op::LdaUndefined, &[]),
                }
                if self.trys.iter().any(|t| t.finally.is_some()) {
                    // The value waits in a register while the `finally`
                    // blocks run.
                    let mark = self.temp;
                    let r = self.alloc(s.pos)?;
                    self.star(r);
                    self.leave_trys(0)?;
                    self.ldar(r);
                    self.emit(Op::Return, &[]);
                    self.reenter_trys(0);
                    self.temp = mark;
                } else {
                    self.emit(Op::Return, &[]);
                }
            }
            StmtKind::Break(label) | StmtKind::Continue(label) => {
                let brk = matches!(s.kind, StmtKind::Break(_));
                let target = self.targets.iter().rev().find(|t| match (label, brk) {
                    (Some(l), _) => t.labels.contains(l),
                    (None, true) => t.breakable,
                    (None, false) => t.cont.is_some(),
                });
                let l = match target {
                    Some(t) if brk => t.brk,
                    Some(Target { cont: Some(c), .. }) => *c,
                    _ => {
                        let what = if brk { "break" } else { "continue" };
                        return Err(Error::new(
                            s.pos as usize,
                            format!("no target for `{what}`"),
                        ));
                    }
                };
                // The `try` statements inside the target, left on the way.
                let t = target.map(|t| t as *const Target).unwrap();
                let at = self.targets.iter().position(|x| std::ptr::eq(x, t)).unwrap();
                let depth = self.trys.iter().position(|c| c.targets > at).unwrap_or(self.trys.len());
                if self.trys[depth..].iter().any(|c| c.finally.is_some()) {
                    self.leave_trys(depth)?;
                    self.jump(l);
                    self.reenter_trys(depth);
                } else {
                    self.jump(l);
                }
            }
            StmtKind::Block(b) => {
                self.enter_scope(b, s.pos)?;
                for s in b {
                    self.stmt(s)?;
                }
            }
            StmtKind::Labeled(name, body) => {
                let mut labels = labels;
                labels.push(name.clone());
                if matches!(
                    body.kind,
                    StmtKind::For { .. }
                        | StmtKind::While(..)
                        | StmtKind::DoWhile(..)
                        | StmtKind::ForIn { .. }
                        | StmtKind::Labeled(..)
                ) {
                    self.pending_labels = labels;
                    self.stmt(body)?;
                } else {
                    let brk = self.label();
                    self.targets.push(Target {
                        labels,
                        brk,
                        cont: None,
                        breakable: false,
                    });
                    self.stmt(body)?;
                    self.targets.pop();
                    self.bind(brk);
                }
            }
        }
        self.temp = mark;
        Ok(())
    }

    /// `switch`: the discriminant in a temporary, then a test of each case
    /// in order, jumping to its statements (`JumpIfStrictEqualSmi` for an
    /// int32 literal, which keeps the discriminant in `acc`), then a jump
    /// to `default` or past the end; the statements follow in order, each
    /// falling into the next.
    fn switch(
        &mut self,
        labels: Vec<Name>,
        d: &Expr,
        cases: &'a [(Option<Expr>, Vec<Stmt>)],
    ) -> R<()> {
        // A local discriminant tested only against literals can't change
        // before the jump: its register serves.
        let literal_tests = cases
            .iter()
            .all(|(t, _)| t.as_ref().is_none_or(|t| smi(t).is_some()));
        let dr = match self.local(d) {
            Some(r) if literal_tests => r,
            _ => {
                self.value(d)?;
                let t = self.alloc(d.pos)?;
                self.star(t);
                t
            }
        };
        for (_, body) in cases {
            self.enter_scope(body, d.pos)?;
        }
        let bodies: Vec<L> = cases.iter().map(|_| self.label()).collect();
        let end = self.label();
        for ((test, _), &l) in cases.iter().zip(&bodies) {
            let Some(test) = test else { continue };
            match smi(test) {
                Some(k) => {
                    self.ldar(dr);
                    self.compare_jump(Op::JumpIfStrictEqualSmi, Arg::Imm(k), l);
                }
                None => {
                    self.value(test)?;
                    self.emit(Op::TestStrictEqual, &[Arg::Reg(dr)]);
                    self.jump_if(Jmp::True, l);
                }
            }
        }
        match cases.iter().position(|(t, _)| t.is_none()) {
            Some(i) => self.jump(bodies[i]),
            None => self.jump(end),
        }
        self.targets.push(Target {
            labels,
            brk: end,
            cont: None,
            breakable: true,
        });
        for ((_, body), &l) in cases.iter().zip(&bodies) {
            self.bind(l);
            for s in body {
                self.stmt(s)?;
            }
        }
        self.targets.pop();
        self.bind(end);
        Ok(())
    }

    /// `for (x in o) body`: the keys as an array (`ForInKeys`), then a
    /// loop over them with an index in a temporary:
    /// `keys; i = 0; Jump test; body: x = keys[i]; …; continue: i++;
    /// test: keys.length > i, back to body; break:`.
    fn for_in(
        &mut self,
        labels: Vec<Name>,
        name: &str,
        fresh: bool,
        object: &Expr,
        body: &'a Stmt,
        pos: Pos,
    ) -> R<()> {
        self.value(object)?;
        self.emit(Op::ForInKeys, &[]);
        let keys = self.alloc(pos)?;
        self.star(keys);
        let i = self.alloc(pos)?;
        self.lda_smi(0);
        self.star(i);
        let (top, cont, test, brk) = (self.loop_label(), self.label(), self.label(), self.label());
        self.jump(test);
        self.bind(top);
        self.ldar(i);
        let ic = self.auto_ic("[]keyed");
        self.emit(Op::GetKeyed, &[Arg::Reg(keys), ic]);
        if fresh {
            let t = self.alloc(pos)?;
            self.star(t);
            self.fresh_binding(name, false, pos)?;
            self.ldar(t);
            self.init_var(name, pos)?;
            self.temp -= 1;
        } else {
            self.store_var(name, pos)?;
        }
        self.targets.push(Target {
            labels,
            brk,
            cont: Some(cont),
            breakable: true,
        });
        self.stmt(body)?;
        self.targets.pop();
        self.bind(cont);
        self.ldar(i);
        self.emit(Op::Inc, &[]);
        self.star(i);
        self.bind(test);
        self.named(Op::GetNamed, keys, "length");
        self.compare_jump(Op::JumpIfGreater, Arg::Reg(i), top);
        self.bind(brk);
        Ok(())
    }

    /// `let`/`const` declarations: each initialized (`undefined` if not
    /// given a value).
    fn lexical_decls(&mut self, ds: &[(Name, Option<Expr>)]) -> R<()> {
        for (name, init) in ds {
            match init {
                Some(e) => self.value(e)?,
                None => self.emit(Op::LdaUndefined, &[]),
            }
            self.init_var(name, init.as_ref().map_or(0, |e| e.pos))?;
        }
        Ok(())
    }

    fn var_decls(&mut self, ds: &[(Name, Option<Expr>)]) -> R<()> {
        for (name, init) in ds {
            if let Some(e) = init {
                self.value(e)?;
                self.store_var(name, e.pos)?;
            }
        }
        Ok(())
    }

    /// A rotated loop: `[Jump test]; body: …; continue: update; test: …,
    /// jumping back to body; break:`.
    fn looping(
        &mut self,
        labels: Vec<Name>,
        test: Option<&Expr>,
        update: Option<&Expr>,
        body: &'a Stmt,
        jump_to_test: bool,
    ) -> R<()> {
        let fresh = std::mem::take(&mut self.fresh);
        let (top, cont, test_l, brk) =
            (self.loop_label(), self.label(), self.label(), self.label());
        if jump_to_test {
            self.jump(test_l);
        }
        self.bind(top);
        self.targets.push(Target {
            labels,
            brk,
            cont: Some(cont),
            breakable: true,
        });
        self.stmt(body)?;
        self.targets.pop();
        self.bind(cont);
        // `for (let i …)`: closures of this iteration keep its `i`.
        for n in fresh {
            self.fresh_binding(n, false, body.pos)?;
        }
        if let Some(u) = update {
            self.effect(u)?;
        }
        self.bind(test_l);
        match test {
            Some(c) => self.test(c, top, brk, Fall::False)?,
            None => self.jump(top),
        }
        self.bind(brk);
        Ok(())
    }
}

/// Whether `for (init; test; …)`'s first test is known to pass: `init`
/// sets `x` to `a` and `test` is `x < b` (or `<=`, `>`, `>=`), and `a` is
/// in that relation to `b`.
fn first_test_passes(init: Option<&ForInit>, test: &Expr) -> bool {
    let ExprKind::Binary(op @ (BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), x, b) = &test.kind
    else {
        return false;
    };
    let (ExprKind::Ident(x), Some(b)) = (&x.kind, smi(b)) else {
        return false;
    };
    let start = match init {
        // The last initializer must be `x`'s, so no other can change it.
        Some(ForInit::Var(ds) | ForInit::Lexical(_, ds)) => match ds.last() {
            Some((n, Some(e))) if n == x => smi(e),
            _ => None,
        },
        Some(ForInit::Expr(Expr {
            kind: ExprKind::Assign(None, t, e),
            ..
        })) if matches!(&t.kind, ExprKind::Ident(n) if n == x) => smi(e),
        _ => None,
    };
    start.is_some_and(|a| match op {
        BinOp::Lt => a < b,
        BinOp::Le => a <= b,
        BinOp::Gt => a > b,
        _ => a >= b,
    })
}
