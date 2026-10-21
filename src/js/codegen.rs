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
    // Functions' source text (`Function.prototype.toString`).
    let text_source: Rc<str> = match lazy {
        Some((_, s, ..)) => s.clone(),
        None => src.into(),
    };
    let text_at = |pos: Pos| lazy.map_or(pos, |(_, _, at, _)| at(pos));
    let text_of = |f: &Function| {
        let (a, b) = f.text.unwrap_or((f.pos, f.end));
        Some((text_source.clone(), text_at(a), text_at(b)))
    };

    let mut decls: Vec<Vec<&str>> = (0..p.functions.len()).map(|i| declarations(p, i)).collect();
    let boxed = boxed_lexicals(p, &decls);
    // A function with more variables than registers keeps the last ones
    // in a scope object (`%env`, a register; see `Gen::function`).
    let mut overflow: Vec<Vec<&str>> = vec![Vec::new(); p.functions.len()];
    for i in 1..p.functions.len() {
        if p.functions[i].has_eval || p.functions[i].generator || p.functions[i].is_async {
            // Its parameters and `var`s in its scope object, where direct
            // `eval`'s code finds them (block-scoped ones, and the parser's,
            // stay in registers). A generator's, all of them: its frame
            // leaves the stack at each `yield`, its closures can't point
            // into it.
            let f = &p.functions[i];
            let params = f.params.len();
            let mut d: Vec<&str> = decls[i][..params].to_vec();
            d.push(ENV);
            let rest: Vec<&str> = decls[i][params..].iter().copied().filter(|&n| n != ENV).collect();
            // (A function expression's own name, immutable, stays apart.)
            let own = f.name.as_deref().filter(|_| f.binds_name);
            let generator = (f.generator || f.is_async) && !f.has_eval;
            let in_env = |n: &str| {
                if generator {
                    return n != GEN && n != EVAL_SCOPE && n != MODULE_ENV && Some(n) != own && !boxed[i].contains(&n);
                }
                !n.starts_with('%') && !p.lexicals.contains_key(n) && Some(n) != own
            };
            d.extend(rest.iter().copied().filter(|&n| !in_env(n)));
            let env_vars: Vec<&str> = rest.iter().copied().filter(|&n| in_env(n)).collect();
            d.extend(env_vars.iter().copied());
            overflow[i] = decls[i][..params].iter().copied().filter(|&n| in_env(n)).chain(env_vars).collect();
            decls[i] = d;
            continue;
        }
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
                    && f.class_kind == 0
                    && !f.uses_new_target
                    && !f.generator
                    && !f.is_async
                    && !scopes.upvalues[i].contains(&ENV)
                    && !boxed_upvalue(p, &boxed, i, &scopes.upvalues[i])
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
                let mut f = Gen::new(p, &decls, &overflow[i], &overflow, &boxed, &scopes, &names, i, lazy.is_some()).function()?;
                f.closures.iter_mut().for_each(|c| *c = index[*c]);
                (f.line, f.col) = position(p.functions[i].params_pos);
                if i != 0 {
                    f.text = text_of(&p.functions[i]);
                }
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
                    defines: Vec::new(),
                    closures: Vec::new(),
                    constants: Vec::new(),
                    handlers: Vec::new(),
                    compound_keys: Vec::new(),
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
                    text: text_of(f),
                });
            }
        }
    }
    let declared = if root == 0 && !p.eval_locals {
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
    let function_names = if root == 0 && !p.eval_locals {
        p.functions[0]
            .body
            .iter()
            .filter_map(|s| match &s.kind {
                StmtKind::Function(i) => p.functions[*i].name.as_deref().map(|n| original(n).to_string()),
                _ => None,
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
        function_names,
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

/// Per function, its block-scoped variables kept in boxes: a generator's
/// (or an async function's) that nested functions use. Its other variables
/// those use are in its scope object, one for the call, as its frame
/// leaves the stack at each `yield`; but each binding of a block-scoped one
/// (each loop iteration's) is its own. So its register holds a box, an
/// object whose property of its name is the variable: a new one for each
/// binding (see [`Gen::fresh_binding`]), which closures capture as they
/// would the variable (see [`Gen::resolve`]).
fn boxed_lexicals<'a>(p: &'a Program, decls: &[Vec<&'a str>]) -> Vec<Vec<&'a str>> {
    let n = p.functions.len();
    let mut used: Vec<HashSet<&str>> = vec![HashSet::default(); n];
    for c in 1..n {
        let refs = references(&p.functions[c]);
        let mut j = p.functions[c].parent;
        while let Some(k) = j {
            used[k].extend(refs.iter().copied());
            j = p.functions[k].parent;
        }
    }
    (0..n)
        .map(|i| {
            let f = &p.functions[i];
            if i == 0 || !(f.generator || f.is_async) {
                return Vec::new();
            }
            // (Not a module's own: its scope object's, which other modules
            // import from.)
            decls[i]
                .iter()
                .copied()
                .filter(|n| p.lexicals.get(*n).is_some_and(|l| !l.module_top) && used[i].contains(n))
                .collect()
        })
        .collect()
}

/// Whether one of function `i`'s upvalues is an enclosing function's boxed
/// variable (see [`boxed_lexicals`]): it can't compile on its own, lazily.
fn boxed_upvalue(p: &Program, boxed: &[Vec<&str>], i: usize, upvalues: &[&str]) -> bool {
    let mut j = p.functions[i].parent;
    while let Some(k) = j {
        if upvalues.iter().any(|n| boxed[k].contains(n)) {
            return true;
        }
        j = p.functions[k].parent;
    }
    false
}

/// Function `i`'s name, for its code.
/// Where a position of the program is in `src` (through `at`, for a
/// function compiled lazily), as 1-based line and column (bytes), for
/// `CLOISON_JIT_LOG` and `CLOISON_JIT_DUMP_FN` (`scripts/vs-maglev.sh` matches
/// functions by it with V8's, which places them at their parameter list's
/// `(` too). Without either, (0, 0): finding lines scans the source, which
/// a lazy compile would do again for every function.
fn positions<'a>(src: &'a str, at: Option<&'a dyn Fn(Pos) -> Pos>) -> impl Fn(Pos) -> (u32, u32) + 'a {
    static LOGGING: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    let logging = *LOGGING.get_or_init(|| {
        std::env::var_os("CLOISON_JIT_LOG").is_some() || std::env::var_os("CLOISON_JIT_DUMP_FN").is_some()
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
            Some(k) if parent != 0 || p.lexicals.contains_key(*n) || p.eval_locals => (true, k as u8 + 1),
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

/// A generator's own generator object's register (see `Op::InitialYield`).
const GEN: &str = "%gen";

/// The variable holding a direct `eval`'s function's scope object, which
/// the functions in it look names up in first (see `Gen::new`).
const EVAL_SCOPE: &str = "%evalscope";

/// The nearest function enclosing function `i` (itself included, but not
/// the script) that calls `eval` directly.
fn eval_scope(p: &Program, i: usize) -> Option<usize> {
    let mut k = Some(i);
    while let Some(j) = k.filter(|&j| j != 0) {
        if p.functions[j].has_eval {
            return Some(j);
        }
        k = p.functions[j].parent;
    }
    None
}

/// The variables of the functions enclosing function `i` (not the
/// script's, which are globals) a direct `eval` in it sees beyond its
/// own: their parameters, `var`s and functions, innermost first, none an
/// inner one's shadows. (Their block-scoped ones the parser lists where
/// the `eval` is: see `Parser::eval_visible`.)
fn eval_outer_names<'a>(p: &'a Program, decls: &[Decls<'a>], i: usize) -> Vec<&'a str> {
    let mut out: Vec<&'a str> = Vec::new();
    let mut j = p.functions[i].parent;
    // (Through arrows only: the function's `arguments` and `new.target`
    // are an arrow's too.)
    let mut arrows = p.functions[i].arrow;
    while let Some(k) = j.filter(|&k| k != 0) {
        for &n in &decls[k].list {
            let own = matches!(n, "arguments" | "%newtarget");
            if own && arrows && !decls[i].contains(&n) && !out.contains(&n) {
                out.push(n);
                continue;
            }
            if n.contains('%') || n == "arguments" || p.lexicals.contains_key(n) || decls[i].contains(&n) || out.contains(&n) {
                continue;
            }
            out.push(n);
        }
        arrows &= p.functions[k].arrow;
        j = p.functions[k].parent;
    }
    out
}

/// `vm::Code::kind` for `f`.
fn function_kind(f: &Function) -> u8 {
    // (A module's: a generator, async or not, its loader drives: see
    // `module`.)
    if f.module {
        4
    } else if f.is_async && f.generator {
        6
    } else if f.is_async {
        5
    } else if f.generator {
        4
    } else if f.class_kind != 0 {
        f.class_kind
    } else if f.arrow || f.method {
        1
    } else {
        0
    }
}

fn unsupported<T>(pos: Pos, what: impl std::fmt::Display) -> R<T> {
    Err(Error::new(pos as usize, format!("unsupported: {what}")))
}

/// An error where nesting has used up the native stack (see
/// `crate::stack`): called by the functions every level of nesting goes
/// through (as the parser's `deep`).
fn deep(pos: Pos) -> R<()> {
    if crate::stack::low() {
        return Err(Error::new(pos as usize, "too deeply nested".into()));
    }
    Ok(())
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
        let mut refs = references(&p.functions[i]);
        // (What a direct `eval` in it sees of the enclosing functions'.)
        if p.functions[i].has_eval {
            refs.extend(eval_outer_names(p, decls, i));
        }
        // (A direct `eval`'s bindings' function: those too, its upvalues.)
        if p.functions[i].name.as_deref() == Some(EVAL_BINDINGS)
            && let Some(k) = p.functions[i].parent
        {
            refs.extend(eval_outer_names(p, decls, k));
        }
        // (In a direct `eval`'s function: its scope object, and an arrow's
        // `this` for the `eval`.)
        if eval_scope(p, i).is_some_and(|k| k != i) {
            refs.push(EVAL_SCOPE);
        }
        if p.functions[i].has_eval && p.functions[i].arrow {
            refs.push("%this");
        }
        for name in refs {
            // An enclosing function's variable in its scope object: the
            // object's register is what is captured. (An import: the
            // module function's parameter holding it.)
            let mut name = p.imports.get(name).map_or(name, |imp| &*imp.holder);
            let mut env_of = None;
            let mut j = p.functions[i].parent;
            while let Some(k) = j.filter(|&k| k != 0) {
                if decls[i].contains(&name) {
                    break;
                }
                if decls[k].contains(&name) {
                    if overflow[k].contains(&name) {
                        name = ENV;
                        env_of = Some(k);
                    }
                    break;
                }
                j = p.functions[k].parent;
            }
            // (Function `k`'s scope object, though this one has its own, an
            // async function's or a generator's, of the same name: from `k`
            // down, unless a function between has one too, which can't pass
            // it on under that name.)
            if let Some(k) = env_of {
                let mut chain = vec![i];
                let mut j = p.functions[i].parent;
                while let Some(m) = j.filter(|&m| m != k) {
                    chain.push(m);
                    j = p.functions[m].parent;
                }
                if chain[1..].iter().any(|&m| decls[m].contains(&ENV)) {
                    continue;
                }
                for c in chain {
                    if !upvalues[c].contains(&ENV) {
                        upvalues[c].push(ENV);
                    }
                }
                if !captured[k].contains(&ENV) {
                    captured[k].push(ENV);
                }
                continue;
            }
            if decls[i].contains(&name) {
                continue;
            }
            let (mut chain, mut j) = (vec![i], p.functions[i].parent);
            // (The script's own variables: its block-scoped ones, or all
            // of strict `eval` code's.)
            let lexical = p.lexicals.contains_key(name) || p.eval_locals;
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
        Class(c) => {
            c.heritage.iter().for_each(|e| expr_refs(e, out));
            for m in &c.members {
                if let PropKey::Computed(k) = &m.key {
                    expr_refs(k, out);
                }
            }
        }
        SuperCall(ctor, args) => {
            if !forwarded {
                out.push(ctor);
                out.push("%newtarget");
                out.push("%this");
            }
            args.iter().for_each(|e| expr_refs(e, out));
        }
        SuperMember(home, k, this) => {
            if !forwarded {
                out.push(home);
            }
            expr_refs(k, out);
            expr_refs(this, out);
        }
        Bound => {}
        Destructure(p, v, _) => {
            pattern_refs(p, out, forwarded);
            expr_refs(v, out);
        }
        Yield(x, _) => x.iter().for_each(|x| expr_refs(x, out)),
        Await(x) => expr_refs(x, out),
        Object(ps, _) => ps.iter().for_each(|(k, e)| {
            if let PropKey::Computed(k) = k {
                expr_refs(k, out);
            }
            expr_refs(e, out)
        }),
        // (A private name's variable too.)
        Member(o, n) if n.starts_with('#') && !forwarded => {
            out.push(n);
            expr_refs(o, out);
        }
        Member(o, _) | Unary(_, o) | Update(o, ..) | Spread(o) | Chain(o) | Optional(o) => expr_refs(o, out),
        Index(a, b)
        | Binary(_, a, b)
        | Logical(_, a, b)
        | Nullish(a, b)
        | Comma(a, b)
        | Assign(_, a, b)
        | LogicalAssign(_, a, b) => {
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

/// The names pattern `p` assigns and its defaults and keys refer to.
fn pattern_refs<'a>(p: &'a Pattern, out: &mut Vec<&'a str>, forwarded: bool) {
    match p {
        Pattern::Name(n) => {
            if !forwarded {
                out.push(n)
            }
        }
        Pattern::Target(e) => expr_names(e, out, forwarded),
        Pattern::Array(es, rest) => {
            for (p, d) in es.iter().flatten() {
                pattern_refs(p, out, forwarded);
                d.iter().for_each(|d| expr_names(d, out, forwarded));
            }
            if let Some(r) = rest {
                pattern_refs(r, out, forwarded);
            }
        }
        Pattern::Object(ps, rest) => {
            for (k, p, d) in ps {
                if let PatKey::Computed(k) = k {
                    expr_names(k, out, forwarded);
                }
                pattern_refs(p, out, forwarded);
                d.iter().for_each(|d| expr_names(d, out, forwarded));
            }
            if let Some(r) = rest {
                pattern_refs(r, out, forwarded);
            }
        }
    }
}

/// Pattern `p`'s own name `n` (borrowed from the tree).
fn pattern_name<'a>(p: &'a Pattern, n: &str) -> Option<&'a str> {
    match p {
        Pattern::Name(m) if &**m == n => Some(m),
        Pattern::Name(_) | Pattern::Target(_) => None,
        Pattern::Array(es, rest) => es
            .iter()
            .flatten()
            .find_map(|(p, _)| pattern_name(p, n))
            .or_else(|| rest.as_ref().and_then(|r| pattern_name(r, n))),
        Pattern::Object(ps, rest) => ps
            .iter()
            .find_map(|(_, p, _)| pattern_name(p, n))
            .or_else(|| rest.as_ref().and_then(|r| pattern_name(r, n))),
    }
}

/// Whether pattern `p`'s defaults, keys or targets may assign `name`.
fn pattern_assigns(p: &Pattern, name: &str) -> bool {
    match p {
        Pattern::Name(_) => false,
        Pattern::Target(e) => assigns(e, name),
        Pattern::Array(es, rest) => {
            es.iter().flatten().any(|(p, d)| pattern_assigns(p, name) || d.is_some_and(|d| assigns(d, name)))
                || rest.as_ref().is_some_and(|r| pattern_assigns(r, name))
        }
        Pattern::Object(ps, rest) => {
            ps.iter().any(|(k, p, d)| {
                matches!(k, PatKey::Computed(k) if assigns(k, name)) || pattern_assigns(p, name) || d.is_some_and(|d| assigns(d, name))
            }) || rest.as_ref().is_some_and(|r| pattern_assigns(r, name))
        }
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
    // (Its class's fields' function, which it calls.)
    out.extend(f.fields.as_deref());
    out
}

/// Whether `s` is the `try` statement of a statement list with `using`
/// declarations (see `ast::USING_CATCH`).
fn using_try(s: &Stmt) -> bool {
    matches!(&s.kind, StmtKind::Try(_, Some((c, _)), _) if c.starts_with(crate::js::ast::USING_CATCH))
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
            StmtKind::ForOf {
                target, iterable, body, pattern, ..
            } => {
                if let Some(p) = pattern {
                    pattern_refs(p, out, fw);
                }
                expr_refs(target, out);
                expr_refs(iterable, out);
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
    out.extend(p.functions[f].hidden.iter().map(|n| &**n));
    if p.functions[f].uses_new_target {
        out.push("%newtarget");
    }
    if p.functions[f].has_eval && f != 0 {
        out.push(EVAL_SCOPE);
    }
    if p.functions[f].generator || p.functions[f].is_async {
        out.push(GEN);
    }
    // The script's variables are globals, but for its block-scoped ones
    // (and strict `eval` code's).
    if f == 0 && !p.eval_locals {
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
            } => ds.iter().filter(|(n, _)| !n.is_empty()).for_each(|(n, _)| add(n, out)),
            StmtKind::ForOf {
                kind: 1..=3,
                pattern: Some(pat),
                ..
            } => {
                let mut ns = Vec::new();
                pat.names(&mut ns);
                for n in ns {
                    if !out.iter().any(|o| *o == &*n) {
                        // (The names live in the tree: find this one's.)
                        if let Some(n) = pattern_name(pat, &n) {
                            out.push(n);
                        }
                    }
                }
            }
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
            StmtKind::ForOf {
                kind: 1..=3,
                target: Expr {
                    kind: ExprKind::Ident(name),
                    ..
                },
                ..
            } => add(name, out),
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
            // (Its object's variable: `Function::hidden`.)
            StmtKind::With(_, _, body) => stmt(body, p, out),
            StmtKind::For { body, .. }
            | StmtKind::ForIn { body, .. }
            | StmtKind::ForOf { body, .. }
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

/// Whether function `f`'s code may see its `arguments` and parameters
/// aliased (a mapped `arguments` object is slower): it assigns a
/// parameter, or an element of `arguments` (or deletes or defines one),
/// or lets `arguments` go anywhere but to `apply` (which copies it): to a
/// variable, a call, a return. Arrows in it too, whose `arguments` is its.
fn aliases_arguments(p: &Program, f: &Function) -> bool {
    fn is_args(e: &Expr) -> bool {
        matches!(&e.kind, ExprKind::Ident(n) if &**n == "arguments")
    }
    fn expr(e: &Expr, p: &Program, f: &Function) -> bool {
        use ExprKind::*;
        let param = |t: &Expr| matches!(&t.kind, Ident(n) if f.params.contains(n));
        let element = |t: &Expr| matches!(&t.kind, Index(o, _) | Member(o, _) if is_args(o));
        match &e.kind {
            Assign(_, t, v) => param(t) || element(t) || expr(t, p, f) || expr(v, p, f),
            Update(t, ..) => param(t) || element(t) || expr(t, p, f),
            Unary(UnOp::Delete, t) if element(t) => true,
            Call(c, args) if args.first().is_some_and(is_args) && matches!(&c.kind, Member(_, m) if m.starts_with("define")) => true,
            Destructure(..) => true,
            // (Reading it, or its elements, aliases nothing; `apply` copies.)
            Index(o, k) if is_args(o) => expr(k, p, f),
            Member(o, _) if is_args(o) => false,
            Call(c, args) if matches!(&c.kind, Member(_, m) if &**m == "apply") && args.len() == 2 && is_args(&args[1]) => {
                expr(c, p, f) || expr(&args[0], p, f)
            }
            Ident(n) if &**n == "arguments" => true,
            Function(i) if p.functions[*i].arrow => {
                let mut found = false;
                for_each_expr(&p.functions[*i].body, &mut |e| found |= expr(e, p, f));
                found
            }
            _ => {
                let mut sub = Vec::new();
                children(e, &mut sub);
                sub.into_iter().any(|x| expr(x, p, f))
            }
        }
    }
    let mut found = false;
    for_each_expr(&f.body, &mut |e| found |= expr(e, p, f));
    found
}

/// Whether running `body` may delete a property: a `delete` in it, or code
/// it calls (a getter's or a setter's aside).
fn may_delete(body: &Stmt) -> bool {
    fn expr(e: &Expr) -> bool {
        use ExprKind::*;
        match &e.kind {
            Unary(UnOp::Delete, _) | Call(..) | New(..) | SuperCall(..) | Yield(..) | Await(..) | Spread(_) | Destructure(..)
            | Intrinsic(..) => true,
            _ => {
                let mut sub = Vec::new();
                children(e, &mut sub);
                sub.into_iter().any(expr)
            }
        }
    }
    let mut found = false;
    for_each_expr(std::slice::from_ref(body), &mut |e| found |= expr(e));
    found
}

/// The expressions directly in `e`.
fn children<'a>(e: &'a Expr<'a>, out: &mut Vec<&'a Expr<'a>>) {
    use ExprKind::*;
    match &e.kind {
        Array(es) | Template(_, es) | Intrinsic(_, es) | SuperCall(_, es) => out.extend(es.iter()),
        Object(ps, _) => {
            for (k, v) in ps {
                if let PropKey::Computed(k) = k {
                    out.push(k);
                }
                out.push(v);
            }
        }
        Member(o, _) | Unary(_, o) | Update(o, ..) | Spread(o) | Chain(o) | Optional(o) => out.push(o),
        Index(a, b)
        | Binary(_, a, b)
        | Logical(_, a, b)
        | Nullish(a, b)
        | Comma(a, b)
        | Assign(_, a, b)
        | LogicalAssign(_, a, b) => {
            out.push(a);
            out.push(b);
        }
        Call(c, args) | New(c, args) => {
            out.push(c);
            out.extend(args.iter());
        }
        Cond(a, b, c) => out.extend([*a, *b, *c]),
        SuperMember(_, k, t) => out.extend([*k, *t]),
        Yield(Some(x), _) => out.push(x),
        Destructure(_, v, _) => out.push(v),
        Class(c) => out.extend(c.heritage.iter()),
        _ => {}
    }
}

/// `f` on each expression statement's, condition's, … expression in
/// `body` (not nested functions').
fn for_each_expr<'a>(body: &'a [Stmt<'a>], f: &mut dyn FnMut(&'a Expr<'a>)) {
    fn stmt<'a>(s: &'a Stmt<'a>, f: &mut dyn FnMut(&'a Expr<'a>)) {
        match &s.kind {
            StmtKind::Var(ds) | StmtKind::Lexical(_, ds) => ds.iter().flat_map(|(_, e)| e).for_each(&mut *f),
            StmtKind::Expr(e) | StmtKind::Throw(e) => f(e),
            StmtKind::Return(e) => e.iter().for_each(&mut *f),
            StmtKind::If(c, a, b) => {
                f(c);
                stmt(a, f);
                b.iter().for_each(|b| stmt(b, f));
            }
            StmtKind::For { init, test, update, body } => {
                match init {
                    Some(ForInit::Var(ds) | ForInit::Lexical(_, ds)) => ds.iter().flat_map(|(_, e)| e).for_each(&mut *f),
                    Some(ForInit::Expr(e)) => f(e),
                    None => {}
                }
                test.iter().chain(update).for_each(&mut *f);
                stmt(body, f);
            }
            StmtKind::ForIn { object, body, .. } => {
                f(object);
                stmt(body, f);
            }
            StmtKind::ForOf { target, iterable, body, .. } => {
                f(target);
                f(iterable);
                stmt(body, f);
            }
            StmtKind::While(c, body) | StmtKind::DoWhile(body, c) | StmtKind::With(c, _, body) => {
                f(c);
                stmt(body, f);
            }
            StmtKind::Block(b) => b.iter().for_each(|s| stmt(s, f)),
            StmtKind::Switch(d, cases) => {
                f(d);
                for (t, b) in cases {
                    t.iter().for_each(&mut *f);
                    b.iter().for_each(|s| stmt(s, f));
                }
            }
            StmtKind::Labeled(_, body) => stmt(body, f),
            StmtKind::Try(b, c, fin) => try_blocks(b, c, fin).for_each(|s| stmt(s, f)),
            _ => {}
        }
    }
    body.iter().for_each(|s| stmt(s, f));
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

/// [`hoisted`]'s, but those with a binding of their own (made by
/// `Gen::enter_scope`).
fn hoisted_vars(p: &Program, s: &Stmt, out: &mut Vec<usize>) {
    if let StmtKind::Function(i) = &s.kind
        && p.functions[*i].binding.is_none()
    {
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

/// An int32 literal (`-0` is not one), or an int32 an expression of them
/// and integer operators makes (`(1 << 16) - 1`: folded, not computed at
/// each run, as PdfJS's `decrypt` did in its loop).
fn smi(e: &Expr) -> Option<i32> {
    if let ExprKind::Binary(op, l, r) = &e.kind {
        let (a, b) = (smi(l)?, smi(r)?);
        let k = b as u32 & 31;
        return match op {
            BinOp::Add => a.checked_add(b),
            BinOp::Sub => a.checked_sub(b),
            // (Not -0: `0 * -1`.)
            BinOp::Mul => a.checked_mul(b).filter(|&p| p != 0 || (a >= 0 && b >= 0)),
            BinOp::Shl => Some(a.wrapping_shl(k)),
            BinOp::Sar => Some(a >> k),
            BinOp::Shr => i32::try_from((a as u32) >> k).ok(),
            BinOp::BitAnd => Some(a & b),
            BinOp::BitOr => Some(a | b),
            BinOp::BitXor => Some(a ^ b),
            _ => None,
        };
    }
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
        Assign(_, t, v) | LogicalAssign(_, t, v) => {
            matches!(&t.kind, Ident(n) if &**n == name) || assigns(t, name) || assigns(v, name)
        }
        Update(t, ..) => matches!(&t.kind, Ident(n) if &**n == name) || assigns(t, name),
        Num(_) | Str(_) | Bool(_) | Null | Ident(_) | This | Function(_) | Accessor(..) | RegExp(..) | Hole
        | TemplateObject(_) => false,
        // (A direct `eval` may assign the bindings it sees.)
        Intrinsic("%eval_scope", es) => es.iter().any(|e| matches!(&e.kind, Ident(n) if &**n == name)),
        Array(es) | Template(_, es) | Intrinsic(_, es) | SuperCall(_, es) => es.iter().any(|e| assigns(e, name)),
        Class(c) => {
            c.heritage.as_ref().is_some_and(|e| assigns(e, name))
                || c.members.iter().any(|m| matches!(&m.key, PropKey::Computed(k) if assigns(k, name)))
        }
        SuperMember(_, k, t) => assigns(k, name) || assigns(t, name),
        Bound => false,
        Yield(x, _) => x.is_some_and(|x| assigns(x, name)),
        Await(x) => assigns(x, name),
        Destructure(p, v, _) => {
            let mut ns = Vec::new();
            p.names(&mut ns);
            ns.iter().any(|n| &**n == name) || assigns(v, name) || pattern_assigns(p, name)
        }
        Object(ps, _) => ps
            .iter()
            .any(|(k, e)| matches!(k, PropKey::Computed(k) if assigns(k, name)) || assigns(e, name)),
        Member(o, _) | Spread(o) | Chain(o) | Optional(o) => assigns(o, name),
        Unary(_, x) => assigns(x, name),
        Index(a, b) | Binary(_, a, b) | Logical(_, a, b) | Nullish(a, b) | Comma(a, b) => {
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
            | StmtKind::ForOf { body, .. }
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
    /// A module's imported binding (see `ast::Import`).
    Import,
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
    /// A `for (… of …)` loop: its `try` (see `TryCtx::close`).
    inner_try: Option<usize>,
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
    /// The slots of `ics` that define properties (see `FunctionDef::defines`).
    defines: Vec<u16>,
    /// The optional chains being compiled: where each one's evaluation
    /// goes once a part of it is null or undefined.
    chains: Vec<L>,
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
    /// Every function's.
    overflows: &'a [Vec<&'a str>],
    /// Every function's variables in boxes (see [`boxed_lexicals`]).
    boxed: &'a [Vec<&'a str>],
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
    /// See `FunctionDef::compound_keys`.
    compound_keys: Vec<(u32, u32)>,
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
    /// The statement that has cleared the completion value (so its own
    /// match arm doesn't again; nested ones do, each completing as
    /// `undefined` unless something in it says otherwise).
    cleared: *const Stmt<'a>,
    /// How many statements enclose the one being compiled: 1 for the
    /// function body's own.
    depth: u32,
    /// A `for (let …)` loop's captured variables, for [`Gen::looping`] to
    /// give each iteration its own.
    fresh: Vec<&'a str>,
    /// Compiling the function's entry (see [`Gen::fresh_binding`]).
    entering: bool,
    /// Compiling its parameters' defaults (a direct `eval` there may not
    /// declare `arguments`).
    in_params: bool,
}

/// A `try` being compiled: the bytecode ranges it covers (closed around
/// the copies of `finally` blocks that jumps out of it run), its
/// `finally` block, and how many jump targets enclose it.
struct TryCtx<'a> {
    start: u32,
    ranges: Vec<(u32, u32)>,
    finally: Option<&'a [Stmt<'a>]>,
    targets: usize,
    /// A `for (… of …)` loop's: the register of the iterator to close when
    /// a jump leaves it (its "finally").
    close: Option<u8>,
    /// A `for await` loop's: the close awaited.
    close_async: bool,
}

impl<'a> Gen<'a> {
    fn new(
        p: &'a Program<'a>,
        decls: &'a [Decls<'a>],
        overflow: &'a [&'a str],
        overflows: &'a [Vec<&'a str>],
        boxed: &'a [Vec<&'a str>],
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
            decls[index]
                .iter()
                .enumerate()
                .filter(|(_, n)| !overflow.contains(n))
                .map(|(i, &n)| (n, i as u8 + 1))
                .collect()
        };
        // (Past the registers of the variables: some of the scope object's
        // may be parameters, at the start.)
        // (A name may be declared twice, as a hidden variable and as what
        // binds it, a `catch`'s: its registers are counted, not the names.)
        let temp = if index == 0 {
            1 + decls[0].len()
        } else {
            1 + decls[index].iter().rposition(|n| !overflow.contains(n)).map_or(0, |k| k + 1)
        };
        Gen {
            p,
            decls,
            scopes,
            index,
            f,
            asm: Assembler::new(),
            labels: Vec::new(),
            ics: Vec::new(),
            defines: Vec::new(),
            chains: Vec::new(),
            names,
            closures: Vec::new(),
            constants: Vec::new(),
            // (Each variable's register, used or not: a closure's upvalue
            // may point to one the function itself never names, which the
            // call must still clear.)
            nregs: temp.max(1 + f.params.len()),
            locals,
            overflow,
            overflows,
            boxed,
            shared_ics: HashMap::default(),
            temp,
            acc: Acc::default(),
            reachable: true,
            targets: Vec::new(),
            pending_labels: Vec::new(),
            deferred: Vec::new(),
            handlers: Vec::new(),
            compound_keys: Vec::new(),
            in_try: 0,
            // (A direct `eval`'s function: its scope object first, as a
            // `with`'s, for the names the eval'd code may declare.)
            withs: {
                let mut w = f.withs.clone();
                if let Some(k) = eval_scope(p, index) {
                    let at = p.functions[k].withs.len().min(w.len());
                    w.insert(at, EVAL_SCOPE.into());
                }
                w
            },
            eval,
            completion: None,
            cleared: std::ptr::null(),
            depth: 0,
            fresh: Vec::new(),
            entering: false,
            in_params: false,
            trys: Vec::new(),
        }
    }

    fn function(mut self) -> R<FunctionDef> {
        if self.f.params.len() + 1 > LOCAL_REGS {
            return unsupported(self.f.pos, "more parameters than registers");
        }
        // `new.target` first (see `Op::LdaNewTarget`).
        if self.index == 0 && self.p.direct_eval {
            // (`%e0`, `%e1`, then the `with` objects `%w0`….)
            let f = self.f;
            for e in &f.withs {
                let k = match &**e {
                    "%e0" => 0,
                    "%e1" => 1,
                    w => 2 + w[2..].parse::<i32>().unwrap_or(0),
                };
                let mark = self.temp;
                let t = self.alloc(self.f.pos)?;
                self.lda_smi(k);
                self.star(t);
                self.intrinsic("eval_env", t, 1);
                self.temp = mark;
                self.init_var(e, self.f.pos)?;
            }
        }
        if self.f.uses_new_target {
            let Var::Reg(r) = self.resolve("%newtarget", self.f.pos)? else {
                return unsupported(self.f.pos, "`new.target` with this many variables");
            };
            self.emit(Op::LdaNewTarget, &[]);
            self.star(r);
            if self.f.class_kind != 0 {
                self.intrinsic("class_call_check", r, 1);
            }
        }
        // A base class's fields, defined on `this` first.
        if self.f.class_kind == 2
            && let Some(v) = &self.f.fields
        {
            self.init_fields(v, 0, self.f.pos)?;
        }
        // The scope object, with each of its variables (`undefined`, in
        // order: one shape).
        let has_eval = self.f.has_eval && self.index != 0;
        if !self.overflow.is_empty() || has_eval || self.f.generator || self.f.is_async {
            let env = self.locals[ENV];
            if self.f.module {
                // (A module's: its loader's, which other modules read.)
                self.ldar(self.locals[MODULE_ENV]);
            } else if has_eval {
                // (No prototype: no `toString` and such for names to find
                // in it.)
                let cap = self.overflow.len().min(crate::vm::MAX_INLINE_CAPACITY as usize) as i32;
                self.lda_smi(cap);
                self.star(env);
                self.intrinsic("null_object", env, 1);
            } else {
                let cap = self.overflow.len().min(crate::vm::MAX_INLINE_CAPACITY as usize) as u8;
                let ic = self.auto_ic(&format!("[]{}", cap));
                self.emit(Op::CreateObject, &[Arg::Count(cap), ic]);
            }
            self.star(env);
            if has_eval {
                let withs = std::mem::take(&mut self.withs);
                self.ldar(env);
                let r = self.store_raw(EVAL_SCOPE, self.f.pos);
                self.withs = withs;
                r?;
                // (Its `new.target`, which the `eval`'s code reads there:
                // see `Parser::new_expr`.)
                if self.f.uses_new_target && !self.f.arrow {
                    self.load_raw("%newtarget", self.f.pos)?;
                    self.scope_var(env, "%newtarget")?;
                }
            }
            for &n in self.overflow {
                // A parameter's value is in its register.
                match self.f.params.iter().position(|p| &**p == n) {
                    Some(k) => self.ldar(k as u8 + 1),
                    None => self.emit(Op::LdaUndefined, &[]),
                }
                if has_eval {
                    self.scope_var(env, n)?;
                } else {
                    self.named(Op::SetNamed, env, n);
                }
            }
        }
        // Each boxed variable's first box (see `boxed_lexicals`).
        let boxed = self.boxed;
        for &n in &boxed[self.index] {
            let mark = self.temp;
            let t = self.alloc(self.f.pos)?;
            self.emit(Op::LdaUndefined, &[]);
            self.star(t);
            self.new_box(self.locals[n], t, n);
            self.temp = mark;
        }
        if arguments_object(self.p, self.index) {
            // A sloppy function's with simple parameters (in registers):
            // mapped to them.
            let f = self.f;
            let simple = !f.strict
                && !f.arrow
                && !f.rest
                && f.param_stmts == 0
                && !f.has_eval
                && !f.generator
                && !f.params.is_empty()
                && f.params.iter().enumerate().all(|(k, p)| !p.starts_with('%') && !f.params[..k].contains(p))
                // (A closure may assign a parameter it captures.)
                && (aliases_arguments(self.p, f) || f.params.iter().any(|p| self.scopes.captured[self.index].contains(&&**p)));
            let op = if simple { Op::CreateMappedArguments } else { Op::CreateArguments };
            match self.locals.get("arguments") {
                Some(&r) => self.emit(op, &[Arg::Reg(r)]),
                None => {
                    let t = self.alloc(self.f.pos)?;
                    self.emit(Op::CreateArguments, &[Arg::Reg(t)]);
                    self.ldar(t);
                    self.store_var("arguments", self.f.pos)?;
                    self.temp -= 1;
                }
            }
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
        } else if let Some(n) = own_name(self.p, self.index)
            && self.overflow.contains(&n)
            && !self.f.params.iter().any(|p| &**p == n)
            && !explicit_declarations(self.p, self.index).contains(&n)
        {
            self.emit(Op::LdaCallee, &[]);
            self.init_var(n, self.f.pos)?;
        }
        // (A new frame's: no upvalues of its registers to close yet.)
        self.entering = true;
        self.enter_scope(&self.f.body, self.f.pos)?;
        self.entering = false;
        let mut fns = Vec::new();
        self.f.body.iter().for_each(|s| hoisted_vars(self.p, s, &mut fns));
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
            // (Strict `eval` code's are its own: registers, `undefined`.)
            for name in self.script_vars().into_iter().filter(|_| !self.p.direct_eval && !self.p.eval_locals) {
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
                // The script's completion value (printed by `cloison`) is its
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
            // A generator starts once its parameters are set; an async
            // function before (what they throw rejects its promise).
            let start = if self.f.is_async && !self.f.generator { 0 } else { self.f.param_stmts };
            for (k, s) in body.iter().enumerate() {
                if (self.f.generator || self.f.is_async) && k == start {
                    self.initial_yield()?;
                }
                // (Its parameters' defaults first: see `Gen::in_params`.)
                self.in_params = k < self.f.param_stmts;
                self.stmt(s)?;
            }
            if (self.f.generator || self.f.is_async) && body.len() <= start {
                self.initial_yield()?;
            }
            self.in_params = false;
        }
        self.emit(Op::LdaUndefined, &[]);
        self.derived_return(self.f.pos)?;
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
            defines: self.defines,
            closures: self.closures,
            constants: self.constants,
            handlers: self.handlers,
            compound_keys: self.compound_keys,
            upvalues,
            captures,
            line: 0,
            col: 0,
            lazy: None,
            kind: function_kind(self.f),
            length: if self.index == 0 { 0 } else { self.f.length as u32 },
            text: None,
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
                for (n, e) in ds.iter().filter(|(n, _)| !n.is_empty()) {
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
                StmtKind::ForOf { kind, target, body, pattern, .. } => {
                    if *kind == 1
                        && let ExprKind::Ident(name) = &target.kind
                    {
                        init.push(name);
                    }
                    if *kind == 1
                        && let Some(pat) = pattern
                    {
                        let mut ns = Vec::new();
                        pat.names(&mut ns);
                        init.extend(ns.iter().filter_map(|n| pattern_name(pat, n)));
                    }
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

    /// The `GetKeyed` of `ro[acc]` that a read-modify-write's `SetKeyed`
    /// (`keyed_write`) stores back to: its offset.
    fn keyed_read(&mut self, ro: u8) -> u32 {
        let at = self.asm.offset() as u32;
        let ic = self.auto_ic("[]keyed");
        self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
        at
    }

    /// `ro[rk] = acc`, storing back what the `GetKeyed` at `read` read:
    /// an object key converted once for both (`FunctionDef::compound_keys`).
    fn keyed_write(&mut self, ro: u8, rk: u8, read: u32) {
        let at = self.asm.offset() as u32;
        let ic = self.auto_ic("[]keyed");
        self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
        if self.reachable {
            self.compound_keys.push((read, at));
        }
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

    /// `GetNamed`/`SetNamed` of property `name` of a member expression
    /// (`o.name`): a private name's (see `Parser::private_ref`) through
    /// the runtime, with its variable's symbol.
    fn member(&mut self, op: Op, obj: u8, name: &str) -> R<()> {
        if !name.starts_with('#') {
            self.named(op, obj, name);
            return Ok(());
        }
        let mark = self.temp;
        let pos = self.f.pos;
        let o = self.alloc(pos)?;
        let k = self.alloc(pos)?;
        if op == Op::SetNamed {
            let v = self.alloc(pos)?;
            self.star(v);
            self.mov(obj, o);
            self.load_var(name, pos)?;
            self.star(k);
            self.intrinsic("private_set", o, 3);
        } else {
            self.mov(obj, o);
            self.load_var(name, pos)?;
            self.star(k);
            self.intrinsic("private_get", o, 2);
        }
        self.temp = mark;
        Ok(())
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
        if self.withs.is_empty() {
            return false;
        }
        self.withs.len() > self.with_skip(name)
    }

    /// The enclosing `with`s that don't shadow `name`: those outside its
    /// block-scoped binding's declaration (counting a direct `eval`'s
    /// function's scope object, see `Gen::new`).
    fn with_skip(&self, name: &str) -> usize {
        let Some(l) = self.p.lexicals.get(name) else {
            // A function's own variable is closer than the `with`s around
            // the function: those inside it may shadow it.
            let mut k = Some(self.index);
            while let Some(j) = k.filter(|&j| j != 0) {
                if self.decls[j].contains(&name) {
                    return self.p.functions[j].withs.len();
                }
                k = self.p.functions[j].parent;
            }
            return 0;
        };
        // (The eval scope object is outside the block-scoped bindings
        // declared in its function.)
        let inside = eval_scope(self.p, self.index).is_some_and(|k| {
            let mut j = Some(l.func);
            while let Some(q) = j {
                if q == k {
                    return true;
                }
                j = self.p.functions[q].parent;
            }
            false
        });
        l.withs + usize::from(inside)
    }

    /// Entering a scope of statements `stmts`: its block-scoped variables
    /// not initialized (each entry a new binding: the upvalues closures
    /// made of their registers closed first), its functions made.
    fn enter_scope(&mut self, stmts: &'a [Stmt<'a>], pos: Pos) -> R<()> {
        for s in stmts {
            if let StmtKind::Lexical(_, ds) = &s.kind {
                for (n, _) in ds.iter().filter(|(n, _)| !n.is_empty()) {
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
        if self.boxed[self.index].contains(&n) {
            return self.fresh_box(n, hole, pos);
        }
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
                self.emit(Op::LdaHole, &[]);
                self.init_var(n, pos)?;
            }
            _ => {}
        }
        Ok(())
    }

    /// A new box for boxed variable `n` (see [`boxed_lexicals`]): its
    /// register's upvalue closed (closures so far keep the old box), and
    /// the variable `undefined` (`hole`) or its value so far.
    fn fresh_box(&mut self, n: &str, hole: bool, pos: Pos) -> R<()> {
        let r = self.locals[n];
        let mark = self.temp;
        let t = self.alloc(pos)?;
        if hole {
            self.emit(Op::LdaHole, &[]);
        } else {
            self.named(Op::GetNamed, r, n);
        }
        self.star(t);
        if !self.entering {
            self.emit(Op::CloseUpvalue, &[Arg::Reg(r)]);
        }
        self.new_box(r, t, n);
        self.temp = mark;
        Ok(())
    }

    /// A box into register `r` whose variable `n` is register `v`'s value.
    fn new_box(&mut self, r: u8, v: u8, n: &str) {
        let ic = self.auto_ic("[]1");
        self.emit(Op::CreateObject, &[Arg::Count(1), ic]);
        self.star(r);
        self.ldar(v);
        self.named(Op::SetNamed, r, n);
    }

    /// Initialize variable `n` with the accumulator (no `with` object's).
    fn init_var(&mut self, n: &str, pos: Pos) -> R<()> {
        let withs = std::mem::take(&mut self.withs);
        let r = self.store_raw(n, pos);
        self.withs = withs;
        r
    }

    /// Strict code's `delete` that failed (`acc` false) throws.
    /// `delete x`: a property or element (the key in `acc`), a global
    /// (its name in `acc`); a variable isn't deleted (false), and anything
    /// else is just evaluated (true).
    fn delete(&mut self, x: &Expr, pos: Pos) -> R<()> {
        use ExprKind::*;
        match &x.kind {
            Member(o, name) => {
                let mark = self.temp;
                let r = self.reg(o, &[])?;
                self.string(name);
                self.emit(Op::DeleteProperty, &[Arg::Reg(r)]);
                self.strict_delete_check(pos)?;
                self.temp = mark;
            }
            Index(o, k) => {
                let mark = self.temp;
                let r = self.reg(o, &[k])?;
                self.value(k)?;
                self.emit(Op::DeleteProperty, &[Arg::Reg(r)]);
                self.strict_delete_check(pos)?;
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
            // `delete a?.b`: true if the chain ends early.
            Chain(c) => {
                let (nil, end) = (self.label(), self.label());
                self.chains.push(nil);
                let r = self.delete(c, pos);
                self.chains.pop();
                r?;
                self.jump(end);
                self.bind(nil);
                self.emit(Op::LdaTrue, &[]);
                self.bind(end);
            }
            _ => {
                self.effect(x)?;
                self.emit(Op::LdaTrue, &[]);
            }
        }
        Ok(())
    }

    /// Call the function in variable `v` (a class's fields' function: see
    /// `Class::fields`) with register `this` as `this`.
    fn init_fields(&mut self, v: &str, this: u8, pos: Pos) -> R<()> {
        let mark = self.temp;
        let f = self.alloc(pos)?;
        self.load_var(v, pos)?;
        self.star(f);
        let t = self.alloc(pos)?;
        self.mov(this, t);
        let ic = self.auto_ic(Op::Call.name());
        self.emit(Op::Call, &[Arg::Reg(f), Arg::Reg(t), Arg::Count(0), ic]);
        self.temp = mark;
        Ok(())
    }

    /// In an optional chain, the value in `acc` (kept) ending it if null
    /// or undefined.
    fn chain_check(&mut self, pos: Pos) -> R<()> {
        let mark = self.temp;
        let t = self.alloc(pos)?;
        self.star(t);
        self.emit(Op::TestNullish, &[]);
        let nil = *self.chains.last().expect("an optional part outside a chain");
        self.jump_if(Jmp::True, nil);
        self.ldar(t);
        self.temp = mark;
        Ok(())
    }

    /// `t &&= v` (`kind` 0), `t ||= v` (1) or `t ??= v` (2): `v` assigned
    /// unless `t`'s value decides the result, which is then that value.
    fn logical_assign(&mut self, kind: u8, t: &Expr, v: &Expr, pos: Pos) -> R<()> {
        let mark = self.temp;
        let end = self.label();
        // (The target's value in `acc`.)
        let decide = |g: &mut Self| -> R<()> {
            match kind {
                0 => g.jump_if(Jmp::ToBooleanFalse, end),
                1 => g.jump_if(Jmp::ToBooleanTrue, end),
                _ => {
                    let old = g.alloc(pos)?;
                    g.star(old);
                    g.emit(Op::TestNullish, &[]);
                    let set = g.label();
                    g.jump_if(Jmp::True, set);
                    g.ldar(old);
                    g.jump(end);
                    g.bind(set);
                }
            }
            Ok(())
        };
        match &t.kind {
            ExprKind::Member(o, name) => {
                let ro = self.reg(o, &[v])?;
                self.member(Op::GetNamed, ro, name)?;
                decide(self)?;
                self.value(v)?;
                self.member(Op::SetNamed, ro, name)?;
            }
            ExprKind::Index(o, k) => {
                let ro = self.reg(o, &[k, v])?;
                let rk = self.reg(k, &[v])?;
                self.ldar(rk);
                let read = self.keyed_read(ro);
                decide(self)?;
                self.value(v)?;
                self.keyed_write(ro, rk, read);
            }
            _ => {
                self.value(t)?;
                decide(self)?;
                self.assign(None, t, v, pos)?;
            }
        }
        self.bind(end);
        self.temp = mark;
        Ok(())
    }

    fn strict_delete_check(&mut self, pos: Pos) -> R<()> {
        if !self.f.strict {
            return Ok(());
        }
        let ok = self.label();
        self.jump_if(Jmp::True, ok);
        let mark = self.temp;
        let t = self.alloc(pos)?;
        self.intrinsic("throw_delete", t, 0);
        self.temp = mark;
        self.bind(ok);
        self.emit(Op::LdaTrue, &[]);
        Ok(())
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

    /// The accumulator stored to variable `name` of scope object `env`, a
    /// direct `eval`'s function's: its own bindings can't be deleted from
    /// an `eval` in it (those an `eval` declares can).
    fn scope_var(&mut self, env: u8, name: &str) -> R<()> {
        let (m, pos) = (self.temp, self.f.pos);
        let a = self.alloc(pos)?;
        let k = self.alloc(pos)?;
        let v = self.alloc(pos)?;
        self.star(v);
        self.mov(env, a);
        self.string(name);
        self.star(k);
        self.intrinsic("scope_var", a, 3);
        self.temp = m;
        Ok(())
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
            // (A `finally` block completing normally leaves the
            // statement's completion value as it was.)
            // (One leaving it abruptly, its own: `undefined` unless it says
            // otherwise.)
            let saved = match self.completion {
                Some(cv) => {
                    let r = self.alloc(pos)?;
                    self.mov(cv, r);
                    self.emit(Op::LdaUndefined, &[]);
                    self.star(cv);
                    Some((cv, r))
                }
                None => None,
            };
            self.enter_scope(f, pos)?;
            for s in f {
                self.stmt(s)?;
            }
            if let Some((cv, r)) = saved
                && self.reachable
            {
                self.mov(r, cv);
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
        if let Some(cv) = self.completion {
            self.emit(Op::LdaUndefined, &[]);
            self.star(cv);
        }
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
            close: None,
            close_async: false,
        });
    }

    /// The innermost `try`'s range ends here, for what follows until
    /// [`Self::resume_try`] to be outside it.
    fn pause_try(&mut self) {
        let end = self.asm.offset() as u32;
        let t = self.trys.last_mut().expect("a try");
        if end > t.start {
            t.ranges.push((t.start, end));
        }
    }

    /// The innermost `try`'s range goes on from here.
    fn resume_try(&mut self) {
        self.trys.last_mut().expect("a try").start = self.asm.offset() as u32;
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
            let (close, finally, close_async) = (t.close, t.finally, t.close_async);
            if let Some(r) = close {
                if close_async {
                    // (Compiled as outside the loop: the `await` may return.)
                    let inner = self.trys.split_off(i);
                    self.intrinsic("async_iterator_close", r, 1);
                    let r = self.yield_acc(self.f.pos);
                    self.trys.extend(inner);
                    r?;
                } else {
                    self.intrinsic("iterator_close", r, 1);
                }
            }
            if let Some(f) = finally {
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
        if !self.p.imports.is_empty() && self.p.imports.contains_key(name) {
            return Ok(Var::Import);
        }
        if let Some(&r) = self.locals.get(name) {
            // (A box's property: see `boxed_lexicals`.)
            if self.boxed[self.index].contains(&name) {
                return Ok(Var::Scope(r));
            }
            return Ok(Var::Reg(r));
        }
        if self.overflow.contains(&name) {
            return Ok(Var::Scope(self.locals[ENV]));
        }
        // An enclosing function's variable in its scope object, through
        // that object's register (an upvalue).
        let mut f = self.f.parent;
        while let Some(k) = f.filter(|&k| k != 0) {
            if self.decls[k].position(name).is_some() {
                // (In its scope object: see `generate`.)
                if self.overflows[k].contains(&name)
                    && let Some(u) = self.scopes.upvalues[self.index].iter().position(|&n| n == ENV)
                {
                    return Ok(Var::ScopeUpvalue(u as u8));
                }
                // (In its box, the upvalue.)
                if self.boxed[k].contains(&name)
                    && let Some(u) = self.scopes.upvalues[self.index].iter().position(|&n| n == name)
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
            ExprKind::This if self.f.class_kind != 3 => Some(0),
            // (In a `with`, a name may be the object's.)
            ExprKind::Ident(n) if !self.in_with(n) && !self.needs_check(n, e.pos) && !self.boxed[self.index].contains(&&**n) => {
                self.locals.get(&**n).copied()
            }
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
        let skip = self.with_skip(name);
        let withs = std::mem::take(&mut self.withs);
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
            // (A `with`'s object, unlike an `eval`'s scope, leaves out what
            // its `Symbol.unscopables` names: `[k, r]`.)
            if w.starts_with("%w") {
                self.intrinsic("with_has", k, 2);
            } else {
                self.emit(Op::TestIn, &[Arg::Reg(k)]);
            }
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

    /// `name = v` inside `with` statements: the reference resolved first
    /// (the object that has `name`, or `HOLE` for its own binding), then
    /// `v`, then the store there.
    fn with_assign(&mut self, name: &str, v: &Expr, pos: Pos) -> R<()> {
        let mark = self.temp;
        let target = self.alloc(pos)?;
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.mov(r, target);
                Ok(())
            },
            &mut |g| {
                g.emit(Op::LdaHole, &[]);
                g.star(target);
                Ok(())
            },
        )?;
        self.bind(end);
        let val = self.alloc(pos)?;
        self.value(v)?;
        self.star(val);
        let hole = self.alloc(pos)?;
        self.emit(Op::LdaHole, &[]);
        self.star(hole);
        let (own, done) = (self.label(), self.label());
        self.ldar(target);
        self.emit(Op::TestStrictEqual, &[Arg::Reg(hole)]);
        self.jump_if(Jmp::True, own);
        if self.f.strict {
            // (Strict code: the property gone meanwhile is an error.)
            let m = self.temp;
            let a = self.alloc(pos)?;
            self.mov(target, a);
            let k = self.alloc(pos)?;
            self.string(original(name));
            self.star(k);
            let v = self.alloc(pos)?;
            self.mov(val, v);
            self.intrinsic("with_set_strict", a, 3);
            self.temp = m;
        } else {
            self.ldar(val);
            self.named(Op::SetNamed, target, original(name));
        }
        self.jump(done);
        self.bind(own);
        self.ldar(val);
        let withs = std::mem::take(&mut self.withs);
        let r = self.store_var(name, pos);
        self.withs = withs;
        r?;
        self.bind(done);
        self.ldar(val);
        self.temp = mark;
        Ok(())
    }

    /// `name op= v` inside `with` statements: the reference resolved once,
    /// read and written in the object that has `name`.
    fn with_compound(&mut self, name: &str, op: BinOp, v: &Expr, pos: Pos) -> R<()> {
        // (`v` is evaluated inside all the `with`s, wherever `name` is.)
        let full = self.withs.clone();
        let apply = |g: &mut Self| -> R<()> {
            let inner = std::mem::replace(&mut g.withs, full.clone());
            let r = g.apply(op, v, pos);
            g.withs = inner;
            r
        };
        let end = self.with_lookup(
            name,
            pos,
            &mut |g, r| {
                g.named(Op::GetNamed, r, original(name));
                apply(g)?;
                if g.f.strict {
                    // (Strict code: the property gone meanwhile is an error.)
                    let m = g.temp;
                    let val = g.alloc(pos)?;
                    g.star(val);
                    let a = g.alloc(pos)?;
                    g.mov(r, a);
                    let k = g.alloc(pos)?;
                    g.string(original(name));
                    g.star(k);
                    let v2 = g.alloc(pos)?;
                    g.mov(val, v2);
                    g.intrinsic("with_set_strict", a, 3);
                    g.temp = m;
                } else {
                    g.named(Op::SetNamed, r, original(name));
                }
                Ok(())
            },
            &mut |g| {
                g.value_inner(&Expr {
                    kind: ExprKind::Ident(name.into()),
                    pos,
                })?;
                apply(g)?;
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

    /// Whether `name` here is a named function expression's own name: the
    /// function's, or an enclosing one's, with no parameter or `var` of
    /// that name in it or in the functions between. (The function by the
    /// name found first, by names alone: compiling stores it is on.)
    fn own_name_binding(&self, name: &str) -> bool {
        let named = |g: usize| {
            let f = &self.p.functions[g];
            f.binds_name && f.name.as_deref() == Some(name)
        };
        let mut g = self.index;
        let top = loop {
            if g == 0 {
                return false;
            }
            if named(g) {
                break g;
            }
            match self.p.functions[g].parent {
                Some(q) => g = q,
                None => return false,
            }
        };
        let mut g = self.index;
        loop {
            let f = &self.p.functions[g];
            if f.params.iter().any(|p| &**p == name) || explicit_declarations(self.p, g).contains(&name) {
                return false;
            }
            if g == top {
                return true;
            }
            g = f.parent.unwrap();
        }
    }

    /// Store the accumulator in variable `name`: a `const` throws, a
    /// block-scoped one not initialized yet too.
    fn store_var(&mut self, name: &str, pos: Pos) -> R<()> {
        if self.in_with(name) {
            return self.with_store(name, pos);
        }
        // A function expression's own name is immutable: a store does
        // nothing, in strict code throws.
        if self.own_name_binding(name) {
            if self.f.strict {
                let mark = self.temp;
                let r = self.alloc(pos)?;
                self.string(name);
                self.star(r);
                self.intrinsic("throw_const", r, 1);
                self.temp = mark;
            }
            return Ok(());
        }
        if !self.p.lexicals.is_empty() && let Some(l) = self.p.lexicals.get(name) {
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
            // (A constant: see `Parser::import_declaration`.)
            Var::Import => {
                let mark = self.temp;
                let r = self.alloc(pos)?;
                self.string(&self.p.imports[name].original.clone());
                self.star(r);
                self.intrinsic("throw_const", r, 1);
                self.temp = mark;
            }
        }
        Ok(())
    }

    // ---- Expressions ----

    /// `e` into a register: a local variable's own one if nothing in
    /// `later` (evaluated after it, before its use) may assign it, or a new
    /// temporary, which the caller frees.
    fn reg(&mut self, e: &Expr, later: &[&Expr]) -> R<u8> {
        match &e.kind {
            ExprKind::This if self.f.class_kind != 3 => return Ok(0),
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
        deep(e.pos)?;
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
            This if self.f.class_kind == 3 && self.f.uses_this => self.load_var("%this", e.pos)?,
            This if self.f.class_kind == 3 => {
                self.ldar(0);
                let k = self.const_str("this");
                self.emit(Op::CheckHole, &[Arg::Index(k)]);
            }
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
        if n == "%this" && self.derived_this() {
            let k = self.const_str("this");
            self.emit(Op::CheckHole, &[Arg::Index(k)]);
        }
        if self.needs_check(n, pos) {
            let k = self.const_str(&self.p.lexicals[n].original.clone());
            self.emit(Op::CheckHole, &[Arg::Index(k)]);
        }
        if let Some(imp) = self.p.imports.get(n).filter(|imp| imp.prop.is_some()) {
            let k = self.const_str(&imp.original.clone());
            self.emit(Op::CheckHole, &[Arg::Index(k)]);
        }
        Ok(())
    }

    /// Whether a reference to `name` at `pos` may find it not initialized:
    /// a block-scoped variable referred to before its declaration's end, or
    /// from a function that may run before it (a declaration, or one made
    /// before it), or in a `switch` (which may jump past it).
    fn needs_check(&self, name: &str, pos: Pos) -> bool {
        if self.p.lexicals.is_empty() {
            return false;
        }
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
                // Its holder's property, live (a hole until its module
                // sets it: `load_var` checks), or the holder itself (a
                // namespace).
                Var::Import => {
                    let imp = &self.p.imports[n];
                    let (holder, prop) = (imp.holder.clone(), imp.prop.clone());
                    self.load_raw(&holder, pos)?;
                    if let Some(prop) = prop {
                        let mark = self.temp;
                        let env = self.alloc(pos)?;
                        self.star(env);
                        self.named(Op::GetNamed, env, &prop);
                        self.temp = mark;
                    }
                }
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
            Class(c) => self.class(c, e.pos)?,
            Yield(Some(x), true) if self.f.is_async => self.async_yield_star(x, e.pos)?,
            Yield(Some(x), true) => self.yield_star(x, e.pos)?,
            // An `await`: a `yield` the promises the coroutine waits on
            // resume (see `generator::async_step`).
            Await(x) => {
                self.value(x)?;
                self.yield_acc(e.pos)?;
            }
            Yield(x, _) => {
                match x {
                    Some(x) => self.value(x)?,
                    None => self.emit(Op::LdaUndefined, &[]),
                }
                if self.f.is_async {
                    // An async generator's: the value awaited, then yielded
                    // as one (see `generator::async_gen_step`).
                    self.yield_acc(e.pos)?;
                    let mark = self.temp;
                    let t = self.alloc(e.pos)?;
                    self.star(t);
                    let g = self.alloc(e.pos)?;
                    self.load_var(GEN, e.pos)?;
                    self.star(g);
                    self.intrinsic("async_gen_yield", g, 1);
                    self.ldar(t);
                    self.temp = mark;
                }
                self.yield_acc(e.pos)?;
            }
            Bound => unreachable!("a declaration's marker"),
            Destructure(p, v, mode) => {
                let mark = self.temp;
                let r = self.alloc(e.pos)?;
                self.into(v, r)?;
                self.destructure(p, r, *mode, e.pos)?;
                self.ldar(r);
                self.temp = mark;
            }
            SuperCall(ctor, args) => {
                let mark = self.temp;
                let c = self.alloc(e.pos)?;
                self.load_var(ctor, e.pos)?;
                self.star(c);
                let nt = self.alloc(e.pos)?;
                self.load_var("%newtarget", e.pos)?;
                self.star(nt);
                let a = self.alloc(e.pos)?;
                self.args_array(args, a, e.pos)?;
                // (`this` the constructor's `%this`, an arrow's too.)
                let t = self.alloc(e.pos)?;
                self.load_raw("%this", e.pos)?;
                self.star(t);
                self.intrinsic("super_call", c, 4);
                if !self.f.arrow {
                    self.star(0);
                }
                self.store_var("%this", e.pos)?;
                // The class's fields, defined on the new `this`.
                if let Some(v) = &self.f.fields {
                    self.star(t);
                    self.init_fields(v, t, e.pos)?;
                    self.ldar(t);
                }
                self.temp = mark;
            }
            SuperMember(home, k, this) => {
                let mark = self.temp;
                let h = self.alloc(e.pos)?;
                self.load_var(home, e.pos)?;
                self.star(h);
                let kr = self.alloc(e.pos)?;
                let t = self.alloc(e.pos)?;
                // (`this` resolved before the key is evaluated.)
                self.into(this, t)?;
                self.into(k, kr)?;
                self.intrinsic("super_get", h, 3);
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
            Object(props, home) => {
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
                if let Some(h) = home {
                    // (Its methods' `super`: see `Gen::class`.)
                    let mark = self.temp;
                    let o = self.alloc(e.pos)?;
                    self.star(o);
                    self.fresh_binding(h, false, e.pos)?;
                    self.ldar(o);
                    self.init_var(h, e.pos)?;
                    self.ldar(o);
                    self.temp = mark;
                }
                if !props.is_empty() {
                    let mark = self.temp;
                    let o = self.alloc(e.pos)?;
                    self.star(o);
                    for (key, v) in props {
                        if let Spread(x) = &v.kind {
                            // `[o, source]` for the runtime.
                            let mark = self.temp;
                            let t = self.alloc(e.pos)?;
                            self.mov(o, t);
                            let sr = self.alloc(e.pos)?;
                            self.into(x, sr)?;
                            self.intrinsic("copy_data_properties", t, 2);
                            self.temp = mark;
                            continue;
                        }
                        let name = match key {
                            PropKey::Name(n) => n,
                            PropKey::Computed(k) => {
                                // `[o, k, v]` for the runtime.
                                let mark = self.temp;
                                let t = self.alloc(e.pos)?;
                                self.mov(o, t);
                                let kr = self.alloc(e.pos)?;
                                self.into(k, kr)?;
                                // (ToPropertyKey before the value.)
                                self.intrinsic("property_key", kr, 1);
                                self.star(kr);
                                let vr = self.alloc(e.pos)?;
                                let which = match v.kind {
                                    Accessor(set, f) => {
                                        self.closure(f);
                                        if set { "define_setter" } else { "define_getter" }
                                    }
                                    // (An anonymous function or class named
                                    // after the key.)
                                    Function(f) if self.p.functions[f].name.is_none() => {
                                        self.value(v)?;
                                        "define_named_field"
                                    }
                                    Class(ref c) if self.p.functions[c.constructor].name.is_none() => {
                                        self.value(v)?;
                                        "define_named_field"
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
                            // `__proto__: v`: the prototype set (not
                            // through `Object.prototype`'s accessor).
                            None if &**name == "__proto__" => {
                                let mark = self.temp;
                                let t = self.alloc(e.pos)?;
                                self.mov(o, t);
                                let vr = self.alloc(e.pos)?;
                                self.into(v, vr)?;
                                self.intrinsic("literal_proto", t, 2);
                                self.temp = mark;
                            }
                            // (Defined: not through a prototype's setter.)
                            None => {
                                self.value(v)?;
                                let ic = self.ic("SetNamed define", name);
                                if let Arg::Index(k) = ic {
                                    self.defines.push(k);
                                }
                                self.emit(Op::SetNamed, &[Arg::Reg(o), ic]);
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
                self.member(Op::GetNamed, r, name)?;
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
            Unary(UnOp::Delete, x) => self.delete(x, e.pos)?,
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
            Nullish(l, r) => {
                let mark = self.temp;
                let (rhs, end) = (self.label(), self.label());
                self.value(l)?;
                let t = self.alloc(e.pos)?;
                self.star(t);
                self.emit(Op::TestNullish, &[]);
                self.jump_if(Jmp::True, rhs);
                self.ldar(t);
                self.jump(end);
                self.bind(rhs);
                self.temp = mark;
                self.value(r)?;
                self.bind(end);
            }
            LogicalAssign(kind, t, v) => self.logical_assign(*kind, t, v, e.pos)?,
            Chain(x) => {
                let (nil, end) = (self.label(), self.label());
                self.chains.push(nil);
                let r = self.value(x);
                self.chains.pop();
                r?;
                self.jump(end);
                self.bind(nil);
                self.emit(Op::LdaUndefined, &[]);
                self.bind(end);
            }
            Optional(x) => {
                self.value(x)?;
                self.chain_check(e.pos)?;
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
        // (`**` has no opcode: a runtime helper's.)
        if op == BinOp::Exp {
            let mark = self.temp;
            let t = self.alloc(pos)?;
            self.into(l, t)?;
            let u = self.alloc(pos)?;
            self.into(r, u)?;
            self.intrinsic("exp", t, 2);
            self.temp = mark;
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
        if op == BinOp::Exp {
            let mark = self.temp;
            let t = self.alloc(pos)?;
            self.star(t);
            let u = self.alloc(pos)?;
            self.into(v, u)?;
            self.intrinsic("exp", t, 2);
            self.temp = mark;
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
            ExprKind::Ident(n) if self.in_with(n) => self.with_assign(n, v, pos),
            ExprKind::Ident(n) => {
                match (op, self.resolve(n, t.pos)?) {
                    (None, _) => self.value(v)?,
                    // `x op= v` with `x` in a register: `v`, then `Op r_x`.
                    // (Not if `v` may change `x` first: a closure capturing
                    // it, or `arguments` aliasing it, in a call `v` makes.)
                    (Some(op), Var::Reg(r))
                        if smi(v).and_then(|k| Self::smi_form(op, k)).is_none()
                            && !self.needs_check(n, t.pos)
                            && Self::reg_form(op).is_some()
                            && !assigns(v, n)
                            && !self.scopes.captured[self.index].contains(&&**n)
                            && !(arguments_object(self.p, self.index) && self.f.params.iter().any(|p| **p == **n)) =>
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
            ExprKind::SuperMember(home, k, this) => {
                let mark = self.temp;
                let h = self.alloc(pos)?;
                self.load_var(home, pos)?;
                self.star(h);
                let kr = self.alloc(pos)?;
                let vr = self.alloc(pos)?;
                let tr = self.alloc(pos)?;
                // (`this` resolved before the key is evaluated.)
                self.into(this, tr)?;
                self.into(k, kr)?;
                match op {
                    None => self.value(v)?,
                    Some(op) => {
                        let m2 = self.temp;
                        let g = self.alloc(pos)?;
                        self.mov(h, g);
                        let g2 = self.alloc(pos)?;
                        self.mov(kr, g2);
                        let g3 = self.alloc(pos)?;
                        self.mov(tr, g3);
                        self.intrinsic("super_get", g, 3);
                        self.temp = m2;
                        self.apply(op, v, pos)?;
                    }
                }
                self.star(vr);
                // `[home, key, value, this]`
                let order = self.alloc(pos)?;
                let o2 = self.alloc(pos)?;
                let o3 = self.alloc(pos)?;
                let o4 = self.alloc(pos)?;
                self.mov(h, order);
                self.mov(kr, o2);
                self.mov(vr, o3);
                self.mov(tr, o4);
                // (Sloppy code's failure silent.)
                let name = if self.f.strict { "super_set" } else { "super_set_sloppy" };
                self.intrinsic(name, order, 4);
                self.temp = mark;
                Ok(())
            }
            ExprKind::Member(o, name) => {
                let ro = self.reg(o, &[v])?;
                match op {
                    None => self.value(v)?,
                    Some(op) => {
                        self.member(Op::GetNamed, ro, name)?;
                        self.apply(op, v, pos)?;
                    }
                }
                self.member(Op::SetNamed, ro, name)?;
                Ok(())
            }
            ExprKind::Index(o, k) => {
                let ro = self.reg(o, &[k, v])?;
                let rk = self.reg(k, &[v])?;
                match op {
                    None => self.value(v)?,
                    Some(op) => {
                        self.ldar(rk);
                        let read = self.keyed_read(ro);
                        self.apply(op, v, pos)?;
                        self.keyed_write(ro, rk, read);
                        return Ok(());
                    }
                }
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
                Ok(())
            }
            ExprKind::Call(..) => self.invalid_target(t),
            _ => unreachable!("the parser checks assignment targets"),
        }
    }

    /// `++t` or `--t` (`d` = ±1), leaving the new value in the accumulator.
    /// `++t` / `--t`; with `postfix`, `t++` / `t--` whose value is used: the
    /// old value, as a number, ends in `acc`.
    /// A call as an assignment's target (sloppy code, Annex B): made,
    /// then a `ReferenceError`.
    fn invalid_target(&mut self, call: &Expr) -> R<()> {
        self.effect(call)?;
        let first = self.temp as u8;
        self.intrinsic("throw_invalid_target", first, 0);
        Ok(())
    }

    fn update(&mut self, t: &Expr, d: i8, postfix: bool, pos: Pos) -> R<()> {
        let mark = self.temp;
        let old = if postfix {
            Some(self.alloc(pos)?)
        } else {
            None
        };
        let step = |g: &mut Self| {
            if let Some(o) = old {
                g.emit(Op::ToNumeric, &[]);
                g.star(o);
            }
            g.emit(if d > 0 { Op::Inc } else { Op::Dec }, &[]);
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
                        if g.f.strict {
                            // (Strict code: the property gone meanwhile is an
                            // error.)
                            let m = g.temp;
                            let val = g.alloc(pos)?;
                            g.star(val);
                            let a = g.alloc(pos)?;
                            g.mov(r, a);
                            let k = g.alloc(pos)?;
                            g.string(original(n));
                            g.star(k);
                            let v2 = g.alloc(pos)?;
                            g.mov(val, v2);
                            g.intrinsic("with_set_strict", a, 3);
                            g.temp = m;
                        } else {
                            g.named(Op::SetNamed, r, original(n));
                        }
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
                self.member(Op::GetNamed, ro, name)?;
                step(self);
                self.member(Op::SetNamed, ro, name)?;
            }
            ExprKind::Index(o, k) => {
                let ro = self.reg(o, &[k])?;
                let rk = self.reg(k, &[])?;
                self.ldar(rk);
                let read = self.keyed_read(ro);
                step(self);
                self.keyed_write(ro, rk, read);
            }
            ExprKind::SuperMember(home, k, this) => {
                // `[home, key, this]` read, then `[home, key, v, this]` set.
                let h = self.alloc(pos)?;
                self.load_var(home, pos)?;
                self.star(h);
                let kr = self.alloc(pos)?;
                let tr = self.alloc(pos)?;
                self.into(this, tr)?;
                self.into(k, kr)?;
                self.intrinsic("super_get", h, 3);
                step(self);
                let order = self.alloc(pos)?;
                let o2 = self.alloc(pos)?;
                let o3 = self.alloc(pos)?;
                let o4 = self.alloc(pos)?;
                self.star(o3);
                self.mov(h, order);
                self.mov(kr, o2);
                self.mov(tr, o4);
                let name = if self.f.strict { "super_set" } else { "super_set_sloppy" };
                self.intrinsic(name, order, 4);
            }
            ExprKind::Call(..) => self.invalid_target(t)?,
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
        // `o.m?.()`: the method (with `o` its `this`) checked.
        let (f, optional) = match &f.kind {
            ExprKind::Optional(m) if !new && matches!(m.kind, ExprKind::Member(..) | ExprKind::Index(..)) => {
                (&**m, true)
            }
            _ => (f, false),
        };
        // (A script's: where it sees lexical bindings, its own `let`s and
        // the like, which an ordinary call of the global `eval` wouldn't.)
        let script_lexicals = || {
            matches!(args.last(), Some(Expr { kind: ExprKind::Intrinsic("%eval_scope", v), .. })
                if v.iter().any(|e| matches!(e.kind, ExprKind::Str(_))))
        };
        if let ExprKind::Ident(n) = &f.kind
            && &**n == "eval"
            && !new
            && self.f.has_eval
            // (A sloppy script's own, with no block-scoped names, an
            // indirect one's; a strict script's has its own `var`s.)
            && (self.index != 0 || self.p.direct_eval || self.f.strict || script_lexicals())
            && !args.iter().any(|a| matches!(a.kind, ExprKind::Spread(_)))
        {
            // Direct `eval`: `[eval, source, scope object, this, strict,
            // in parameters, scope for its declarations, with objects…]`.
            // The scope object it looks names up in: the function's (with
            // its parameters and `var`s), under one of the other bindings
            // it sees (block-scoped ones, enclosing functions' variables),
            // accessors of the variables themselves (or, for one the
            // bindings' function has no upvalue of, a copy whose value the
            // binding takes back after).
            let (args, visible): (&[Expr], &[Expr]) = match args.split_last() {
                Some((Expr { kind: ExprKind::Intrinsic("%eval_scope", v), .. }, rest)) => (rest, v),
                _ => (args, &[]),
            };
            // (Its bindings' function last: see `Parser::eval_bindings`.)
            let (carrier, visible) = match visible.split_last() {
                Some((Expr { kind: ExprKind::Function(f), .. }, rest)) => (Some(*f), rest),
                _ => (None, visible),
            };
            let mark = self.temp;
            let fr = self.alloc(pos)?;
            self.into(f, fr)?;
            let src = self.alloc(pos)?;
            match args.first() {
                Some(a) => self.into(a, src)?,
                None => {
                    self.emit(Op::LdaUndefined, &[]);
                    self.star(src);
                }
            }
            for a in args.iter().skip(1) {
                self.effect(a)?;
            }
            // A strict script's own: an indirect one's, strict (its
            // `var`s its own, no `new.target`, the global names).
            if self.index == 0 && !self.p.direct_eval && !script_lexicals() {
                self.intrinsic("strict_eval", fr, 2);
                self.temp = mark;
                return Ok(());
            }
            let env = self.alloc(pos)?;
            if self.index == 0 && self.p.direct_eval {
                // (`eval` in `eval`'s code: the same scope.)
                self.load_raw("%e0", pos)?;
                self.star(env);
            } else if self.index == 0 {
                // (A script's: its `var`s are the global object's, `this`.)
                self.mov(0, env);
            } else {
                self.mov(self.locals[ENV], env);
            }
            let this = self.alloc(pos)?;
            if self.f.arrow {
                self.load_var("%this", pos)?;
            } else {
                self.ldar(0);
            }
            self.star(this);
            let strict = self.alloc(pos)?;
            self.emit(if self.f.strict { Op::LdaTrue } else { Op::LdaFalse }, &[]);
            self.star(strict);
            let params = self.alloc(pos)?;
            // (An arrow has no `arguments` of its own: unless a parameter
            // has the name, the eval's may be declared.)
            let restricted = self.in_params && (!self.f.arrow || self.f.params.iter().any(|p| &**p == "arguments"));
            self.emit(if restricted { Op::LdaTrue } else { Op::LdaFalse }, &[]);
            self.star(params);
            let decl = self.alloc(pos)?;
            self.mov(env, decl);
            // The objects of the `with` statements around it in the
            // function (or in the `eval`'s code, and those around that
            // `eval`), whose properties its names find first.
            let withs: Vec<Name> = match self.withs.iter().position(|w| &**w == EVAL_SCOPE || &**w == "%e1") {
                Some(at) => self.withs[..at]
                    .iter()
                    .filter(|w| w.starts_with("%w"))
                    .chain(&self.withs[at + 1..])
                    .cloned()
                    .collect(),
                None => Vec::new(),
            };
            for w in &withs {
                let r = self.alloc(pos)?;
                self.load_raw(w, pos)?;
                self.star(r);
            }
            let nargs = (7 + withs.len()) as u8;
            // The bindings: the parser's, then the enclosing functions'
            // variables none of those shadows (none with no variable here).
            // (Block-scoped ones first, then the enclosing functions':
            // those a `var` of the function's by the name, which an `eval`
            // may declare, shadows.)
            // Each binding's: a kind (see `intrinsics::eval_scope`), and the
            // index of the upvalue of the bindings' function that is it (an
            // accessor of the scope object reads and writes it), if any.
            let mut bindings: Vec<(&str, &str, i32)> = visible
                .chunks(3)
                .filter_map(|t| match (&t[0].kind, &t[1].kind, &t[2].kind) {
                    // (An enclosing function's, past the function's own
                    // variables: to the `eval` as an enclosing function's
                    // variable, which a `var` it declares shadows.)
                    (ExprKind::Str(o), ExprKind::Ident(v), ExprKind::Num(k)) if *k as u8 & EVAL_OUTER != 0 => {
                        Some((o.as_str(), &**v, (*k as u8 & !EVAL_OUTER) as i32))
                    }
                    (ExprKind::Str(o), ExprKind::Ident(v), ExprKind::Num(k)) => {
                        Some((o.as_str(), &**v, *k as i32 | EVAL_BLOCK_SCOPED as i32))
                    }
                    _ => None,
                })
                .collect();
            // (Block-scoped first: an enclosing function's after.)
            bindings.sort_by_key(|b| b.2 & EVAL_BLOCK_SCOPED as i32 == 0);
            let lexical = bindings.iter().filter(|b| b.2 & EVAL_BLOCK_SCOPED as i32 != 0).count();
            for n in eval_outer_names(self.p, self.decls, self.index) {
                if !bindings.iter().any(|(o, ..)| *o == n) {
                    bindings.push((n, n, 0));
                }
            }
            let mut kept = Vec::new();
            let mut nlex = 0;
            for (i, b) in bindings.into_iter().enumerate() {
                if !matches!(self.resolve(b.1, pos)?, Var::Global) {
                    let up = carrier.and_then(|f| self.scopes.upvalues[f].iter().position(|&u| u == b.1));
                    // (One in a box: see `boxed_lexicals`.)
                    let mut k = Some(self.index);
                    while let Some(j) = k.filter(|&j| !self.decls[j].contains(&b.1)) {
                        k = self.p.functions[j].parent;
                    }
                    let boxed = k.is_some_and(|j| self.boxed[j].contains(&b.1));
                    kept.push((b.0, b.1, b.2 | if boxed { EVAL_BOXED as i32 } else { 0 }, up));
                    nlex += (i < lexical) as i32;
                }
            }
            kept.truncate(80);
            let nlex = nlex.min(kept.len() as i32);
            if kept.is_empty() {
                self.intrinsic("direct_eval", fr, nargs);
                self.temp = mark;
                return Ok(());
            }
            let scope = self.alloc(pos)?;
            let m2 = self.temp;
            self.mov(env, scope);
            let n = self.alloc(pos)?;
            self.emit(Op::LdaSmi, &[Arg::Imm(nlex)]);
            self.star(n);
            let c = self.alloc(pos)?;
            match carrier {
                Some(f) if kept.iter().any(|b| b.3.is_some()) => self.closure(f),
                _ => self.emit(Op::LdaUndefined, &[]),
            }
            self.star(c);
            for &(o, v, kind, up) in &kept {
                let k = self.alloc(pos)?;
                self.string(o);
                self.star(k);
                // (The value of one with no accessor; else the variable's
                // name, a box's property's.)
                let r = self.alloc(pos)?;
                match up {
                    Some(_) => self.string(v),
                    None => self.load_raw(v, pos)?,
                }
                self.star(r);
                let b = self.alloc(pos)?;
                match up {
                    Some(u) => self.emit(Op::LdaSmi, &[Arg::Imm(((u as i32) << 4) | kind)]),
                    None => self.emit(Op::LdaUndefined, &[]),
                }
                self.star(b);
            }
            self.intrinsic("eval_scope", scope, (3 + 3 * kept.len()) as u8);
            self.star(scope);
            self.temp = m2;
            self.mov(scope, env);
            self.intrinsic("direct_eval", fr, nargs);
            let result = self.alloc(pos)?;
            self.star(result);
            // What it stored in the scope object but to the bindings (the
            // function's `var`s it assigned, the ones it declared) goes on
            // to the function's.
            {
                let m3 = self.temp;
                let s = self.alloc(pos)?;
                self.mov(scope, s);
                let d = self.alloc(pos)?;
                self.mov(decl, d);
                let n = self.alloc(pos)?;
                self.emit(Op::LdaSmi, &[Arg::Imm(nlex)]);
                self.star(n);
                for (o, ..) in &kept {
                    let k = self.alloc(pos)?;
                    self.string(o);
                    self.star(k);
                }
                self.intrinsic("eval_scope_flush", s, (3 + kept.len()) as u8);
                self.temp = m3;
            }
            for (i, &(o, v, kind, up)) in kept.iter().enumerate() {
                if kind & EVAL_CONST as i32 != 0 || up.is_some() {
                    continue;
                }
                let m3 = self.temp;
                let s = self.alloc(pos)?;
                self.mov(scope, s);
                let k = self.alloc(pos)?;
                self.string(o);
                self.star(k);
                let cur = self.alloc(pos)?;
                self.load_raw(v, pos)?;
                self.star(cur);
                let d = self.alloc(pos)?;
                if (i as i32) < nlex {
                    self.emit(Op::LdaUndefined, &[]);
                    self.star(d);
                } else {
                    self.mov(decl, d);
                }
                self.intrinsic("eval_scope_get", s, 4);
                // (The binding itself, not a `var` of the `eval`'s by its
                // name, which a function's lookups would find first.)
                let withs = std::mem::take(&mut self.withs);
                let r = self.store_raw(v, pos);
                self.withs = withs;
                r?;
                self.temp = m3;
            }
            self.ldar(result);
            self.temp = mark;
            return Ok(());
        }
        if let ExprKind::SuperMember(_, _, this) = &f.kind
            && !new
        {
            // The method, then a call with `this`.
            let mark = self.temp;
            let c = self.alloc(pos)?;
            self.value(f)?;
            self.star(c);
            if args.iter().any(|a| matches!(a.kind, ExprKind::Spread(_))) {
                let w = self.alloc(pos)?;
                self.into(this, w)?;
                let a = self.alloc(pos)?;
                self.args_array(args, a, pos)?;
                self.intrinsic("call_spread", c, 3);
                self.temp = mark;
                return Ok(());
            }
            let n = u8::try_from(args.len()).or_else(|_| unsupported(pos, "more than 255 arguments"))?;
            let w = self.alloc(pos)?;
            for _ in 0..n {
                self.alloc(pos)?;
            }
            self.into(this, w)?;
            for (i, a) in args.iter().enumerate() {
                self.into(a, w + 1 + i as u8)?;
            }
            let ic = self.auto_ic(Op::Call.name());
            self.emit(Op::Call, &[Arg::Reg(c), Arg::Reg(w), Arg::Count(n), ic]);
            self.temp = mark;
            return Ok(());
        }
        if args.iter().any(|a| matches!(a.kind, ExprKind::Spread(_))) {
            return self.call_spread(f, args, new, optional, pos);
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
                Ok(name) => self.member(Op::GetNamed, ro, name)?,
                Err(k) => {
                    self.value(k)?;
                    let ic = self.auto_ic("[]keyed");
                    self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                }
            }
            self.star(c);
            if optional {
                self.chain_check(pos)?;
            }
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
    fn call_spread(&mut self, f: &Expr, args: &[Expr], new: bool, optional: bool, pos: Pos) -> R<()> {
        let mark = self.temp;
        let c = self.alloc(pos)?;
        let w = if new { c } else { self.alloc(pos)? };
        match &f.kind {
            ExprKind::Member(o, name) if !new => {
                self.into(o, w)?;
                self.member(Op::GetNamed, w, name)?;
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
        if optional {
            self.ldar(c);
            self.chain_check(pos)?;
        }
        let a = self.alloc(pos)?;
        self.args_array(args, a, pos)?;
        if new {
            self.intrinsic("construct_spread", c, 2);
        } else {
            self.intrinsic("call_spread", c, 3);
        }
        self.temp = mark;
        Ok(())
    }

    /// Arguments (some spread) into a new array in register `a`.
    fn args_array(&mut self, args: &[Expr], a: u8, pos: Pos) -> R<()> {
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
        Ok(())
    }

    /// A class: its constructor, its prototype (`class_setup`), its
    /// methods defined on them, its home objects' and inner binding's
    /// variables set; the constructor in `acc`.
    fn class(&mut self, c: &Class, pos: Pos) -> R<()> {
        // (New bindings before the closures capturing them are made.)
        self.fresh_binding(&c.home, false, pos)?;
        self.fresh_binding(&c.home_static, false, pos)?;
        let vars = (c.privates.iter().map(|(v, _)| v))
            .chain(c.fields.iter().map(|(v, _)| v))
            .chain(c.members.iter().filter_map(|m| m.var.as_ref()));
        for v in vars {
            self.fresh_binding(v, false, pos)?;
        }
        // Its private names: a new symbol each.
        for (v, n) in &c.privates {
            let mark = self.temp;
            let t = self.alloc(pos)?;
            self.string(n);
            self.star(t);
            self.intrinsic("private_name", t, 1);
            self.init_var(v, pos)?;
            self.temp = mark;
        }
        // (The inner binding not initialized while the heritage is
        // evaluated: `class x extends x {}` is a ReferenceError.)
        if let Some(inner) = &c.inner {
            self.fresh_binding(inner, true, pos)?;
        }
        let mark = self.temp;
        let h = self.alloc(pos)?;
        match &c.heritage {
            Some(x) => self.value(x)?,
            None => self.emit(Op::LdaHole, &[]),
        }
        self.star(h);
        let f = self.alloc(pos)?;
        self.closure(c.constructor);
        self.star(f);
        self.intrinsic("class_setup", h, 2);
        let p = self.alloc(pos)?;
        self.star(p);
        self.ldar(p);
        self.init_var(&c.home, pos)?;
        self.ldar(f);
        self.init_var(&c.home_static, pos)?;
        for m in &c.members {
            let m4 = self.temp;
            if m.kind == 3 {
                // A field's computed key.
                let PropKey::Computed(e) = &m.key else { unreachable!() };
                let k = self.alloc(pos)?;
                self.into(e, k)?;
                self.intrinsic("property_key", k, 1);
                self.init_var(m.var.as_ref().unwrap(), pos)?;
                self.temp = m4;
                continue;
            }
            if let Some(v) = &m.var {
                // A private method: (a static one's defined on the class.)
                self.closure(m.function);
                self.init_var(v, pos)?;
                if m.is_static {
                    let o = self.alloc(pos)?;
                    self.mov(f, o);
                    let k = self.alloc(pos)?;
                    let PropKey::Name(n) = &m.key else { unreachable!() };
                    let pv = &c.privates.iter().find(|(_, p)| p == n).unwrap().0;
                    self.load_var(pv, pos)?;
                    self.star(k);
                    let fr = self.alloc(pos)?;
                    self.load_var(v, pos)?;
                    self.star(fr);
                    let kind = self.alloc(pos)?;
                    self.lda_smi(m.kind as i32);
                    self.star(kind);
                    self.intrinsic("private_method", o, 4);
                }
                self.temp = m4;
                continue;
            }
            let o = self.alloc(pos)?;
            self.mov(if m.is_static { f } else { p }, o);
            let k = self.alloc(pos)?;
            match &m.key {
                PropKey::Name(n) => self.string(n),
                PropKey::Computed(e) => self.value(e)?,
            }
            self.star(k);
            let fr = self.alloc(pos)?;
            self.closure(m.function);
            self.star(fr);
            let flag = self.alloc(pos)?;
            if matches!(m.key, PropKey::Computed(_)) {
                self.emit(Op::LdaTrue, &[]);
            } else {
                self.emit(Op::LdaFalse, &[]);
            }
            self.star(flag);
            let which = match m.kind {
                0 => "define_method",
                1 => "define_getter",
                _ => "define_setter",
            };
            self.intrinsic(which, o, 4);
            self.temp = m4;
        }
        if let Some((v, init)) = &c.fields {
            self.closure(*init);
            self.init_var(v, pos)?;
        }
        if let Some(inner) = &c.inner {
            self.ldar(f);
            self.init_var(inner, pos)?;
        }
        // Its static fields and blocks, in order, with the class `this`.
        for &s in &c.statics {
            let m = self.temp;
            let fr = self.alloc(pos)?;
            self.closure(s);
            self.star(fr);
            let t = self.alloc(pos)?;
            self.mov(f, t);
            let ic = self.auto_ic(Op::Call.name());
            self.emit(Op::Call, &[Arg::Reg(fr), Arg::Reg(t), Arg::Count(0), ic]);
            self.temp = m;
        }
        self.ldar(f);
        self.temp = mark;
        Ok(())
    }

    /// Destructure register `r` into pattern `p` (`mode` as
    /// `ExprKind::Destructure`'s).
    fn destructure(&mut self, p: &Pattern, r: u8, mode: u8, pos: Pos) -> R<()> {
        deep(pos)?;
        match p {
            Pattern::Name(n) => {
                self.ldar(r);
                if mode == 2 {
                    self.init_var(n, pos)?;
                } else {
                    self.store_var(n, pos)?;
                }
            }
            Pattern::Target(t) => match &t.kind {
                ExprKind::Member(o, name) => {
                    let mark = self.temp;
                    let ro = self.reg(o, &[])?;
                    self.ldar(r);
                    self.member(Op::SetNamed, ro, name)?;
                    self.temp = mark;
                }
                ExprKind::Index(o, k) => {
                    let mark = self.temp;
                    let ro = self.reg(o, &[k])?;
                    let rk = self.reg(k, &[])?;
                    self.ldar(r);
                    let ic = self.auto_ic("[]keyed");
                    self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
                    self.temp = mark;
                }
                _ => return unsupported(pos, "this destructuring target"),
            },
            Pattern::Array(es, rest) => {
                let mark = self.temp;
                let it = self.alloc(pos)?;
                self.mov(r, it);
                self.intrinsic("get_iterator", it, 1);
                self.star(it);
                let hole = self.alloc(pos)?;
                self.emit(Op::LdaHole, &[]);
                self.star(hole);
                let v = self.alloc(pos)?;
                // Where something but a step may throw (a default, a
                // nested pattern, a target), a handler closes the iterator
                // unless done (`it` then `undefined`) and throws on: the
                // steps outside it. (Not otherwise: a function with a
                // handler isn't compiled.)
                let close = es.iter().flatten().any(|(p, d)| self.may_throw(p, *d, mode))
                    || rest.as_ref().is_some_and(|p| self.may_throw(p, None, mode));
                if close {
                    self.open_try(None);
                    self.trys.last_mut().unwrap().close = Some(it);
                    self.in_try += 1;
                }
                for e in es {
                    // (A target's object and key first, then the step.)
                    let target = match e {
                        Some((Pattern::Target(t), _)) => Some(self.target_ref(t, pos)?),
                        _ => None,
                    };
                    let have = self.label();
                    if close {
                        self.pause_try();
                    }
                    self.intrinsic("pattern_step", it, 1);
                    self.star(v);
                    if close {
                        self.resume_try();
                    }
                    self.emit(Op::TestStrictEqual, &[Arg::Reg(hole)]);
                    self.jump_if(Jmp::False, have);
                    // Done: no more steps, and `undefined`.
                    self.emit(Op::LdaUndefined, &[]);
                    self.star(it);
                    self.star(v);
                    self.bind(have);
                    if let Some((p, d)) = e {
                        self.pattern_default(v, *d)?;
                        match target {
                            Some(t) => self.store_target(t, v)?,
                            None => self.destructure(p, v, mode, pos)?,
                        }
                    }
                }
                if let Some(rest) = rest {
                    let target = match &**rest {
                        Pattern::Target(t) => Some(self.target_ref(t, pos)?),
                        _ => None,
                    };
                    if close {
                        self.pause_try();
                    }
                    self.intrinsic("iterator_rest", it, 1);
                    self.star(v);
                    self.emit(Op::LdaUndefined, &[]);
                    self.star(it);
                    if close {
                        self.resume_try();
                    }
                    match target {
                        Some(t) => self.store_target(t, v)?,
                        None => self.destructure(rest, v, mode, pos)?,
                    }
                }
                if close {
                    self.in_try -= 1;
                    let ranges = self.close_try();
                    let end = self.label();
                    if self.reachable {
                        self.jump(end);
                    }
                    let handler = self.label();
                    self.labels[handler.0].used = true;
                    self.reachable = false;
                    self.bind(handler);
                    let at = self.asm.offset() as u32;
                    self.handlers.extend(ranges.iter().map(|&(s, e)| (s, e, at)));
                    let e = self.alloc(pos)?;
                    self.star(e);
                    self.intrinsic("iterator_close_quiet", it, 1);
                    self.ldar(e);
                    self.emit(Op::Throw, &[]);
                    self.bind(end);
                }
                if rest.is_none() {
                    self.intrinsic("iterator_close", it, 1);
                }
                self.temp = mark;
            }
            Pattern::Object(ps, rest) => {
                let mark = self.temp;
                let src = self.alloc(pos)?;
                self.mov(r, src);
                self.intrinsic("require_coercible", src, 1);
                let keys = if rest.is_some() {
                    let k = self.alloc(pos)?;
                    self.emit(Op::CreateArray, &[Arg::Count(ps.len().min(255) as u8)]);
                    self.star(k);
                    Some(k)
                } else {
                    None
                };
                let v = self.alloc(pos)?;
                for (key, p, d) in ps {
                    match key {
                        PatKey::Name(n) => {
                            if let Some(k) = keys {
                                self.string(n);
                                self.emit(Op::PushLiteral, &[Arg::Reg(k)]);
                            }
                            // (An index key is an element's.)
                            match n.parse::<i32>().ok().filter(|i| *i >= 0 && i.to_string() == **n) {
                                Some(i) => {
                                    self.lda_smi(i);
                                    let ic = self.auto_ic("[]keyed");
                                    self.emit(Op::GetKeyed, &[Arg::Reg(src), ic]);
                                }
                                None => self.named(Op::GetNamed, src, n),
                            }
                        }
                        PatKey::Computed(e) => {
                            let m = self.temp;
                            let kr = self.alloc(pos)?;
                            self.into(e, kr)?;
                            if let Some(k) = keys {
                                self.ldar(kr);
                                self.emit(Op::PushLiteral, &[Arg::Reg(k)]);
                            }
                            self.ldar(kr);
                            let ic = self.auto_ic("[]keyed");
                            self.emit(Op::GetKeyed, &[Arg::Reg(src), ic]);
                            self.temp = m;
                        }
                    }
                    self.star(v);
                    self.pattern_default(v, *d)?;
                    self.destructure(p, v, mode, pos)?;
                }
                if let (Some(rest), Some(k)) = (rest, keys) {
                    let m = self.temp;
                    let a = self.alloc(pos)?;
                    self.mov(src, a);
                    let b = self.alloc(pos)?;
                    self.mov(k, b);
                    self.intrinsic("object_rest", a, 2);
                    self.temp = m;
                    self.star(v);
                    self.destructure(rest, v, mode, pos)?;
                }
                self.temp = mark;
            }
        }
        Ok(())
    }

    /// `v = d` if `v` is `undefined` (a pattern element's default).
    /// Whether destructuring `p` (with default `d`, in `mode`: see
    /// [`Self::destructure`]) may throw: a default not a literal, a nested
    /// pattern, a target, an assignment to what isn't a local variable.
    fn may_throw(&self, p: &Pattern, d: Option<&Expr>, mode: u8) -> bool {
        let literal = |e: &Expr| matches!(e.kind, ExprKind::Num(_) | ExprKind::Str(_) | ExprKind::Bool(_) | ExprKind::Null);
        if d.is_some_and(|e| !literal(e)) {
            return true;
        }
        match p {
            Pattern::Name(n) => {
                mode == 0
                    && !(self.locals.contains_key(&**n)
                        && !self.in_with(n)
                        && !self.needs_check(n, 0)
                        && !self.p.lexicals.get(&**n).is_some_and(|l| l.constant))
            }
            _ => true,
        }
    }

    /// An assignment target's object, and key if computed (`o.x`, `o[k]`),
    /// evaluated into temporaries, and the property's name: for
    /// [`Self::store_target`] after.
    fn target_ref(&mut self, t: &Expr, pos: Pos) -> R<(u8, Option<u8>, Option<Name>)> {
        match &t.kind {
            ExprKind::Member(o, name) => {
                let ro = self.alloc(pos)?;
                self.into(o, ro)?;
                Ok((ro, None, Some(name.clone())))
            }
            ExprKind::Index(o, k) => {
                let ro = self.alloc(pos)?;
                self.into(o, ro)?;
                let rk = self.alloc(pos)?;
                self.into(k, rk)?;
                Ok((ro, Some(rk), None))
            }
            _ => unsupported(pos, "this destructuring target"),
        }
    }

    /// `v` stored into a target [`Self::target_ref`] evaluated.
    fn store_target(&mut self, (ro, rk, name): (u8, Option<u8>, Option<Name>), v: u8) -> R<()> {
        self.ldar(v);
        match (rk, name) {
            (Some(rk), _) => {
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
                Ok(())
            }
            (None, Some(n)) => self.member(Op::SetNamed, ro, &n),
            _ => unreachable!(),
        }
    }

    fn pattern_default(&mut self, v: u8, d: Option<&Expr>) -> R<()> {
        let Some(d) = d else { return Ok(()) };
        let skip = self.label();
        self.ldar(v);
        self.emit(Op::TestUndefined, &[]);
        self.jump_if(Jmp::False, skip);
        self.value(d)?;
        self.star(v);
        self.bind(skip);
        Ok(())
    }

    /// `return acc`: through the `finally` blocks being left. (A derived
    /// class's constructor checks what it returns after them, outside its
    /// `try`s: `derived_return`.)
    fn return_acc(&mut self, pos: Pos) -> R<()> {
        if self.trys.iter().any(|t| t.finally.is_some() || t.close.is_some()) || self.f.class_kind == 3 && !self.trys.is_empty() {
            // The value waits in a register while the `finally` blocks run.
            let mark = self.temp;
            let r = self.alloc(pos)?;
            self.star(r);
            self.leave_trys(0)?;
            self.ldar(r);
            self.derived_return(pos)?;
            self.emit(Op::Return, &[]);
            self.reenter_trys(0);
            self.temp = mark;
        } else {
            self.derived_return(pos)?;
            self.emit(Op::Return, &[]);
        }
        Ok(())
    }

    /// A generator's start, once its parameters are set: its generator
    /// object made, and returned (see `Op::InitialYield`).
    fn initial_yield(&mut self) -> R<()> {
        let Var::Reg(g) = self.resolve(GEN, self.f.pos)? else {
            return unsupported(self.f.pos, "a generator with this many variables");
        };
        let op = if self.f.is_async && !self.f.generator { Op::AsyncStart } else { Op::InitialYield };
        self.emit(op, &[Arg::Reg(g)]);
        // (Resumed by `next()`: a fresh accumulator.)
        self.acc = Acc::default();
        self.reachable = true;
        Ok(())
    }

    /// Suspend at a `yield` of `acc`; resumed, the value sent (`next(v)`)
    /// in `acc`, or its exception thrown (`throw(e)`), or a return
    /// (`return(v)`) made, through the `finally` blocks.
    fn yield_acc(&mut self, pos: Pos) -> R<()> {
        let Var::Reg(g) = self.resolve(GEN, pos)? else {
            return unsupported(pos, "a generator with this many variables");
        };
        self.emit(Op::Yield, &[Arg::Reg(g)]);
        self.acc = Acc::default();
        self.reachable = true;
        let mark = self.temp;
        let v = self.alloc(pos)?;
        self.star(v);
        self.intrinsic("generator_mode", g, 1);
        let (thrown, returned, done) = (self.label(), self.label(), self.label());
        self.compare_jump(Op::JumpIfStrictEqualSmi, Arg::Imm(1), thrown);
        self.compare_jump(Op::JumpIfStrictEqualSmi, Arg::Imm(2), returned);
        self.jump(done);
        self.bind(thrown);
        self.ldar(v);
        self.emit(Op::Throw, &[]);
        self.bind(returned);
        self.ldar(v);
        self.return_acc(pos)?;
        self.bind(done);
        self.ldar(v);
        self.temp = mark;
        Ok(())
    }

    /// `yield* x`: each of `x`'s iterator's values yielded, what's sent
    /// passed on to it; its result's value at the end.
    fn yield_star(&mut self, x: &Expr, pos: Pos) -> R<()> {
        let Var::Reg(g) = self.resolve(GEN, pos)? else {
            return unsupported(pos, "a generator with this many variables");
        };
        let mark = self.temp;
        // `[iterator, received, gen]` for `delegate_step`.
        let it = self.alloc(pos)?;
        self.into(x, it)?;
        self.intrinsic("get_iterator", it, 1);
        self.star(it);
        let received = self.alloc(pos)?;
        self.emit(Op::LdaUndefined, &[]);
        self.star(received);
        let gr = self.alloc(pos)?;
        self.mov(g, gr);
        let result = self.alloc(pos)?;
        let (top, done) = (self.loop_label(), self.label());
        self.bind(top);
        self.intrinsic("delegate_step", it, 3);
        self.star(result);
        self.intrinsic("delegate_done", result, 1);
        self.jump_if(Jmp::True, done);
        self.named(Op::GetNamed, result, "value");
        self.emit(Op::Yield, &[Arg::Reg(g)]);
        self.acc = Acc::default();
        self.reachable = true;
        self.star(received);
        self.jump(top);
        self.bind(done);
        // (A `return()` passed on: the generator returns the value.)
        let v = self.alloc(pos)?;
        self.named(Op::GetNamed, result, "value");
        self.star(v);
        let (ret, end) = (self.label(), self.label());
        self.intrinsic("generator_mode", g, 1);
        self.compare_jump(Op::JumpIfStrictEqualSmi, Arg::Imm(2), ret);
        self.jump(end);
        self.bind(ret);
        self.ldar(v);
        self.return_acc(pos)?;
        self.bind(end);
        self.ldar(v);
        self.temp = mark;
        Ok(())
    }

    /// `yield* x` in an async generator: as [`Self::yield_star`] over
    /// `x`'s async iterator, each result awaited (a sync iterator's
    /// values too), the mode a resumption was in kept for the next step.
    fn async_yield_star(&mut self, x: &Expr, pos: Pos) -> R<()> {
        let Var::Reg(g) = self.resolve(GEN, pos)? else {
            return unsupported(pos, "a generator with this many variables");
        };
        let mark = self.temp;
        // `[iterator, received, mode]` for `async_delegate_step`.
        let it = self.alloc(pos)?;
        let received = self.alloc(pos)?;
        let mode = self.alloc(pos)?;
        let sync = self.alloc(pos)?;
        let result = self.alloc(pos)?;
        let v = self.alloc(pos)?;
        let gr = self.alloc(pos)?;
        self.into(x, it)?;
        self.intrinsic("async_iterator_kind", it, 1);
        self.star(sync);
        self.mov(sync, received);
        self.intrinsic("get_async_iterator", it, 2);
        self.star(it);
        self.emit(Op::LdaUndefined, &[]);
        self.star(received);
        self.lda_smi(0);
        self.star(mode);
        let (top, done) = (self.loop_label(), self.label());
        self.bind(top);
        self.intrinsic("async_delegate_step", it, 3);
        self.yield_acc(pos)?;
        self.star(result);
        // The result's value into `v` (a sync iterator's awaited).
        let value = |g: &mut Self| -> R<()> {
            let ready = g.label();
            g.named(Op::GetNamed, result, "value");
            g.star(v);
            g.ldar(sync);
            g.jump_if(Jmp::False, ready);
            g.ldar(v);
            g.yield_acc(pos)?;
            g.star(v);
            g.bind(ready);
            Ok(())
        };
        self.intrinsic("delegate_done", result, 1);
        self.jump_if(Jmp::True, done);
        value(self)?;
        // Yielded as it is; what the generator resumes with (and how) sent
        // on.
        self.mov(g, gr);
        self.intrinsic("async_gen_yield", gr, 1);
        self.ldar(v);
        self.emit(Op::Yield, &[Arg::Reg(g)]);
        self.acc = Acc::default();
        self.reachable = true;
        self.star(received);
        self.mov(g, gr);
        self.intrinsic("generator_mode", gr, 1);
        self.star(mode);
        self.jump(top);
        self.bind(done);
        // (A `return()` passed on: the generator returns the value.)
        value(self)?;
        let (ret, end) = (self.label(), self.label());
        self.ldar(mode);
        self.compare_jump(Op::JumpIfStrictEqualSmi, Arg::Imm(2), ret);
        self.jump(end);
        self.bind(ret);
        self.ldar(v);
        self.return_acc(pos)?;
        self.bind(end);
        self.ldar(v);
        self.temp = mark;
        Ok(())
    }

    /// A derived class's constructor returning `acc`: the object it
    /// returns, else `this` (see `intrinsics::derived_return`).
    fn derived_return(&mut self, pos: Pos) -> R<()> {
        if self.f.class_kind != 3 || !self.reachable {
            return Ok(());
        }
        let mark = self.temp;
        let v = self.alloc(pos)?;
        self.star(v);
        let t = self.alloc(pos)?;
        if self.f.uses_this {
            self.load_raw("%this", pos)?;
            self.star(t);
        } else {
            self.mov(0, t);
        }
        self.intrinsic("derived_return", v, 2);
        self.temp = mark;
        Ok(())
    }

    /// Whether `this` here is a derived class constructor's (not
    /// initialized until `super()` returns).
    fn derived_this(&self) -> bool {
        let mut k = self.index;
        while self.p.functions[k].arrow {
            match self.p.functions[k].parent {
                Some(q) => k = q,
                None => return false,
            }
        }
        self.p.functions[k].class_kind == 3
    }

    // ---- Tests ----

    /// Jump to `t` if `e` is truthy and to `f` otherwise, one of which is
    /// reached by falling through (see [`Fall`]).
    fn test(&mut self, e: &Expr, t: L, f: L, fall: Fall) -> R<()> {
        deep(e.pos)?;
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
        deep(s.pos)?;
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
                    if self.p.direct_eval && self.index == 0 {
                        // (Its `var` is the caller's: in the scope object.)
                        self.store_var(f.name.as_deref().unwrap(), s.pos)?;
                    } else {
                        self.init_var(f.name.as_deref().unwrap(), s.pos)?;
                    }
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
            | StmtKind::ForOf { .. }
            | StmtKind::While(..)
            | StmtKind::DoWhile(..)
            | StmtKind::Switch(..)
            | StmtKind::Try(..)
            | StmtKind::With(..)
                if self.completion.is_some() && !std::ptr::eq(self.cleared, s) && !using_try(s) =>
            {
                self.emit(Op::LdaUndefined, &[]);
                self.star(self.completion.unwrap());
                let outer = std::mem::replace(&mut self.cleared, s);
                self.pending_labels = labels;
                let r = self.stmt_inner(s);
                self.cleared = outer;
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
                // (Each time the statement runs, its own binding: closures in
                // it keep their object, `for (…) with (o) f = () => x`. First:
                // a box's copies its value through `acc`.)
                let saved = std::mem::take(&mut self.withs);
                self.fresh_binding(name, false, s.pos)?;
                self.withs = saved;
                let mark = self.temp;
                let t = self.alloc(s.pos)?;
                self.into(o, t)?;
                self.intrinsic("to_object", t, 1);
                self.temp = mark;
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
            StmtKind::ForOf {
                kind,
                target,
                iterable,
                body,
                pattern,
                is_await,
            } => self.for_of(labels, *kind, target, pattern.as_deref(), iterable, body, *is_await, s.pos)?,
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
                // (An async generator's value awaited.)
                if self.f.is_async && self.f.generator && e.is_some() {
                    self.yield_acc(s.pos)?;
                }
                self.return_acc(s.pos)?;
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
                let mut depth = self.trys.iter().position(|c| c.targets > at).unwrap_or(self.trys.len());
                // (`continue` stays in a `for … of`'s own.)
                if !brk && let Some(k) = self.targets[at].inner_try {
                    depth = depth.max(k + 1);
                }
                if self.trys[depth..].iter().any(|c| c.finally.is_some() || c.close.is_some()) {
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
                        | StmtKind::ForOf { .. }
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
                        inner_try: None,
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
            inner_try: None,
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

    /// `for (target of iterable) body`: the iterator in a register; each
    /// step's value (`HOLE` when done, see `intrinsics::iterator_step`) to
    /// the target, then the body. Inside a `try` of its own whose exits
    /// close the iterator (see `TryCtx::close`), and an exception too.
    fn for_of(
        &mut self,
        labels: Vec<Name>,
        kind: u8,
        target: &Expr,
        pattern: Option<&Pattern>,
        iterable: &Expr,
        body: &'a Stmt,
        is_await: bool,
        pos: Pos,
    ) -> R<()> {
        let it = self.alloc(pos)?;
        let hole = self.alloc(pos)?;
        let v = self.alloc(pos)?;
        // (`for await`: whether over a sync iterator, whose values are
        // awaited instead of its results.)
        let sync = if is_await { Some(self.alloc(pos)?) } else { None };
        // (`let`/`const`: not initialized while the iterable is evaluated.)
        if kind >= 2 {
            let mut ns = Vec::new();
            match (pattern, &target.kind) {
                (Some(p), _) => p.names(&mut ns),
                (None, ExprKind::Ident(n)) => ns.push(n.clone()),
                _ => {}
            }
            for n in &ns {
                self.fresh_binding(n, true, pos)?;
            }
        }
        self.into(iterable, v)?;
        match sync {
            Some(sync) => {
                self.intrinsic("async_iterator_kind", v, 1);
                self.star(sync);
                self.intrinsic("get_async_iterator", v, 2);
            }
            None => self.intrinsic("get_iterator", v, 1),
        }
        self.star(it);
        self.emit(Op::LdaHole, &[]);
        self.star(hole);
        let (top, cont, done, brk) = (self.loop_label(), self.label(), self.label(), self.label());
        self.targets.push(Target {
            labels,
            brk,
            cont: Some(cont),
            breakable: true,
            inner_try: Some(self.trys.len()),
        });
        self.open_try(None);
        self.trys.last_mut().unwrap().close = Some(it);
        self.trys.last_mut().unwrap().close_async = is_await;
        self.in_try += 1;
        self.bind(top);
        // (What the step throws, `next()`, a result's `done` or `value`,
        // doesn't close the iterator: outside the loop's `try`.)
        self.pause_try();
        match sync {
            // `await it.next()` (a sync iterator's result as it is), then
            // its value (a sync iterator's awaited).
            Some(sync) => {
                let (result_ready, value_ready) = (self.label(), self.label());
                self.intrinsic("async_iterator_next", it, 1);
                self.star(v);
                self.ldar(sync);
                self.jump_if(Jmp::True, result_ready);
                self.ldar(v);
                self.yield_acc(pos)?;
                self.star(v);
                self.bind(result_ready);
                self.intrinsic("async_iterator_value", v, 1);
                self.star(v);
                self.ldar(sync);
                self.jump_if(Jmp::False, value_ready);
                // A sync iterator's step, as AsyncFromSyncIterator's: a
                // promise of its value (`undefined` once done) settling a
                // job after it, awaited.
                let not_done = self.label();
                let finished = self.alloc(pos)?;
                self.ldar(v);
                self.emit(Op::TestStrictEqual, &[Arg::Reg(hole)]);
                self.star(finished);
                self.jump_if(Jmp::False, not_done);
                self.emit(Op::LdaUndefined, &[]);
                self.star(v);
                self.bind(not_done);
                self.intrinsic("async_from_sync", v, 1);
                self.yield_acc(pos)?;
                self.star(v);
                self.ldar(finished);
                self.temp -= 1;
                self.jump_if(Jmp::True, done);
                self.bind(value_ready);
                self.ldar(v);
            }
            None => self.intrinsic("iterator_step", it, 1),
        }
        self.star(v);
        self.resume_try();
        self.emit(Op::TestStrictEqual, &[Arg::Reg(hole)]);
        self.jump_if(Jmp::True, done);
        match (kind, &target.kind) {
            _ if pattern.is_some() => {
                let p = pattern.unwrap();
                if kind >= 2 {
                    let mut ns = Vec::new();
                    p.names(&mut ns);
                    for n in &ns {
                        self.fresh_binding(n, false, pos)?;
                    }
                }
                self.destructure(p, v, if kind >= 2 { 2 } else if kind == 1 { 1 } else { 0 }, pos)?;
            }
            (2 | 3, ExprKind::Ident(n)) => {
                self.fresh_binding(n, false, pos)?;
                self.ldar(v);
                self.init_var(n, pos)?;
            }
            (_, ExprKind::Ident(n)) => {
                self.ldar(v);
                self.store_var(n, pos)?;
            }
            (_, ExprKind::Member(o, name)) => {
                let ro = self.reg(o, &[])?;
                self.ldar(v);
                self.member(Op::SetNamed, ro, name)?;
            }
            (_, ExprKind::Index(o, k)) => {
                let ro = self.reg(o, &[k])?;
                let rk = self.reg(k, &[])?;
                self.ldar(v);
                let ic = self.auto_ic("[]keyed");
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk), ic]);
            }
            (_, ExprKind::Call(..)) => self.invalid_target(target)?,
            _ => return unsupported(pos, "this `for … of` target"),
        }
        self.stmt(body)?;
        self.bind(cont);
        self.jump(top);
        self.bind(done);
        self.in_try -= 1;
        let ranges = self.close_try();
        self.targets.pop();
        if self.reachable {
            self.jump(brk);
        }
        // An exception: the iterator closed (its own errors lost), then
        // thrown on.
        let handler = self.label();
        self.labels[handler.0].used = true;
        self.reachable = false;
        self.bind(handler);
        let at = self.asm.offset() as u32;
        self.handlers.extend(ranges.iter().map(|&(s, e)| (s, e, at)));
        let e = self.alloc(pos)?;
        self.star(e);
        self.intrinsic("iterator_close_quiet", it, 1);
        self.ldar(e);
        self.emit(Op::Throw, &[]);
        self.bind(brk);
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
        if fresh {
            self.fresh_binding(name, true, pos)?;
        }
        // A key deleted before its turn isn't visited: checked where the
        // body may delete (see `may_delete`), with the object and the key.
        let check = may_delete(body);
        let pair = if check { Some((self.alloc(pos)?, self.alloc(pos)?)) } else { None };
        self.value(object)?;
        if let Some((o, _)) = pair {
            self.star(o);
        }
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
        if let Some((o, k)) = pair {
            self.star(k);
            self.intrinsic("for_in_has", o, 2);
            self.jump_if(Jmp::False, cont);
            self.ldar(k);
        }
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
            inner_try: None,
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
                Some(Expr { kind: ExprKind::Bound, .. }) => continue,
                Some(e) if name.is_empty() => {
                    self.effect(e)?;
                    continue;
                }
                Some(e) => self.value(e)?,
                None if name.is_empty() => continue,
                None => self.emit(Op::LdaUndefined, &[]),
            }
            self.init_var(name, init.as_ref().map_or(0, |e| e.pos))?;
        }
        Ok(())
    }

    fn var_decls(&mut self, ds: &[(Name, Option<Expr>)]) -> R<()> {
        for (name, init) in ds {
            if let Some(Expr { kind: ExprKind::Bound, .. }) = init {
                continue;
            }
            if name.is_empty() {
                if let Some(e) = init {
                    self.effect(e)?;
                }
                continue;
            }
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
            inner_try: None,
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
