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

use std::collections::HashMap;

use super::Error;
use super::ast::*;
use crate::asm::{Constant, FunctionDef, Module};
use crate::bytecode::{Arg, Assembler, Label, Op};

pub fn generate(p: &Program) -> Result<Module, Error> {
    let decls: Vec<Vec<&str>> = (0..p.functions.len()).map(|i| declarations(p, i)).collect();
    let mut functions = Vec::with_capacity(p.functions.len());
    for i in 0..p.functions.len() {
        functions.push(Gen::new(p, &decls, i).function()?);
    }
    Ok(Module {
        functions,
        script: 0,
    })
}

type R<T> = Result<T, Error>;

fn unsupported<T>(pos: Pos, what: impl std::fmt::Display) -> R<T> {
    Err(Error::new(pos as usize, format!("unsupported: {what}")))
}

/// Every name a function declares: parameters, `var`s and function
/// declarations, in order of declaration.
fn declarations(p: &Program, f: usize) -> Vec<&str> {
    fn stmt<'a>(s: &'a Stmt, p: &'a Program, out: &mut Vec<&'a str>) {
        let add = |n: &'a str, out: &mut Vec<&'a str>| {
            if !out.contains(&n) {
                out.push(n)
            }
        };
        match &s.kind {
            StmtKind::Var(ds)
            | StmtKind::For {
                init: Some(ForInit::Var(ds)),
                ..
            } => ds.iter().for_each(|(n, _)| add(n, out)),
            StmtKind::Function(i) => add(p.functions[*i].name.as_deref().unwrap(), out),
            StmtKind::ForIn {
                var: true, name, ..
            } => add(name, out),
            _ => {}
        }
        match &s.kind {
            StmtKind::If(_, a, b) => {
                stmt(a, p, out);
                if let Some(b) = b {
                    stmt(b, p, out);
                }
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
    let mut out: Vec<&str> = f.params.iter().map(String::as_str).collect();
    for s in &f.body {
        stmt(s, p, &mut out);
    }
    out
}

/// The function declarations directly in a function's body (hoisted to
/// its start), in order.
fn hoisted(s: &Stmt, out: &mut Vec<usize>) {
    match &s.kind {
        StmtKind::Function(i) => out.push(*i),
        StmtKind::If(_, a, b) => {
            hoisted(a, out);
            if let Some(b) = b {
                hoisted(b, out);
            }
        }
        StmtKind::For { body, .. }
        | StmtKind::ForIn { body, .. }
        | StmtKind::While(_, body)
        | StmtKind::DoWhile(body, _)
        | StmtKind::Labeled(_, body) => hoisted(body, out),
        StmtKind::Block(b) => b.iter().for_each(|s| hoisted(s, out)),
        StmtKind::Switch(_, cases) => cases
            .iter()
            .flat_map(|(_, b)| b)
            .for_each(|s| hoisted(s, out)),
        StmtKind::Try(b, c, f) => try_blocks(b, c, f).for_each(|s| hoisted(s, out)),
        _ => {}
    }
}

/// The statements of a `try`'s blocks.
fn try_blocks<'a>(
    b: &'a [Stmt],
    c: &'a Option<(String, Vec<Stmt>)>,
    f: &'a Option<Vec<Stmt>>,
) -> impl Iterator<Item = &'a Stmt> {
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
            matches!(&t.kind, Ident(n) if n == name) || assigns(t, name) || assigns(v, name)
        }
        Update(t, ..) => matches!(&t.kind, Ident(n) if n == name) || assigns(t, name),
        Num(_) | Str(_) | Bool(_) | Null | Ident(_) | This | Function(_) => false,
        Array(es) => es.iter().any(|e| assigns(e, name)),
        Object(ps) => ps.iter().any(|(_, e)| assigns(e, name)),
        Member(o, _) => assigns(o, name),
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
    labels: Vec<String>,
    brk: L,
    /// For loops.
    cont: Option<L>,
    /// A loop or a `switch`: an unlabeled `break` leaves it.
    breakable: bool,
}

/// What the accumulator is known to hold, within a basic block.
#[derive(Default, Clone)]
struct Acc {
    /// Registers holding the same value.
    regs: Vec<u8>,
    smi: Option<i32>,
}

struct Gen<'a> {
    p: &'a Program,
    decls: &'a [Vec<&'a str>],
    index: usize,
    f: &'a Function,
    asm: Assembler,
    labels: Vec<LabelInfo>,
    ics: Vec<String>,
    closures: Vec<usize>,
    constants: Vec<Constant>,
    nregs: usize,
    locals: HashMap<&'a str, u8>,
    /// The next free temporary.
    temp: usize,
    acc: Acc,
    reachable: bool,
    targets: Vec<Target>,
    /// Labels for the statement being compiled (`a: for …`).
    pending_labels: Vec<String>,
    /// Statements compiled at the end of the function: the `then` branches
    /// of `if`s whose condition can only jump when true.
    deferred: Vec<(L, &'a Stmt)>,
    /// Exception handlers: (start, end, handler) bytecode offsets, inner
    /// ones first (see `FunctionDef::handlers`).
    handlers: Vec<(u32, u32, u32)>,
    /// `try` blocks being compiled: none of their statements goes out of
    /// line, outside the handler's range.
    in_try: u32,
}

impl<'a> Gen<'a> {
    fn new(p: &'a Program, decls: &'a [Vec<&'a str>], index: usize) -> Self {
        let f = &p.functions[index];
        // The script's names are globals.
        let locals: HashMap<&str, u8> = if index == 0 {
            HashMap::new()
        } else {
            decls[index]
                .iter()
                .enumerate()
                .map(|(i, &n)| (n, i as u8 + 1))
                .collect()
        };
        let temp = 1 + locals.len();
        Gen {
            p,
            decls,
            index,
            f,
            asm: Assembler::new(),
            labels: Vec::new(),
            ics: Vec::new(),
            closures: Vec::new(),
            constants: Vec::new(),
            nregs: 1 + f.params.len(),
            locals,
            temp,
            acc: Acc::default(),
            reachable: true,
            targets: Vec::new(),
            pending_labels: Vec::new(),
            deferred: Vec::new(),
            handlers: Vec::new(),
            in_try: 0,
        }
    }

    fn function(mut self) -> R<FunctionDef> {
        if self.locals.len() + 1 > 255 {
            return unsupported(self.f.pos, "more than 254 parameters and variables");
        }
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
            for name in self.uninitialized_script_vars() {
                self.emit(Op::LdaUndefined, &[]);
                self.global(Op::StaGlobal, name);
            }
            // The script's completion value is its last expression
            // statement's.
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
            self.stmt(s)?;
        }
        if self.nregs > 256 {
            return unsupported(self.f.pos, "more than 256 registers");
        }
        let name = match (self.index, &self.f.name) {
            (0, _) => "<script>".to_string(),
            (_, Some(n)) => n.clone(),
            (_, None) => "<anonymous>".to_string(),
        };
        Ok(FunctionDef {
            name,
            nparams: self.f.params.len() as u32,
            nregs: self.nregs as u32,
            code: self.asm.finish(),
            ics: self.ics,
            closures: self.closures,
            constants: self.constants,
            handlers: self.handlers,
        })
    }

    /// Script `var`s declared only without an initializer, and not also
    /// functions.
    fn uninitialized_script_vars(&self) -> Vec<&'a str> {
        fn walk<'a>(s: &'a Stmt, init: &mut Vec<&'a str>, bare: &mut Vec<&'a str>) {
            let mut decl = |ds: &'a [(String, Option<Expr>)]| {
                for (n, e) in ds {
                    if e.is_some() { &mut *init } else { &mut *bare }.push(n);
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
                    walk(body, init, bare);
                }
                StmtKind::ForIn {
                    var, name, body, ..
                } => {
                    if *var {
                        init.push(name);
                    }
                    walk(body, init, bare);
                }
                StmtKind::Function(_) => {}
                StmtKind::If(_, a, b) => {
                    walk(a, init, bare);
                    if let Some(b) = b {
                        walk(b, init, bare);
                    }
                }
                StmtKind::For { body, .. }
                | StmtKind::While(_, body)
                | StmtKind::DoWhile(body, _)
                | StmtKind::Labeled(_, body) => walk(body, init, bare),
                StmtKind::Block(b) => b.iter().for_each(|s| walk(s, init, bare)),
                StmtKind::Switch(_, cases) => cases
                    .iter()
                    .flat_map(|(_, b)| b)
                    .for_each(|s| walk(s, init, bare)),
                StmtKind::Try(b, c, f) => try_blocks(b, c, f).for_each(|s| walk(s, init, bare)),
                _ => {}
            }
        }
        let (mut init, mut bare) = (Vec::new(), Vec::new());
        self.f
            .body
            .iter()
            .for_each(|s| walk(s, &mut init, &mut bare));
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
            let extra = usize::from(op == Op::Call);
            self.nregs = self.nregs.max(w as usize + c as usize + extra);
        }
        self.asm.emit(op, args);
        match (op, args) {
            (Op::Ldar, &[Arg::Reg(r)]) => {
                self.acc = Acc {
                    regs: vec![r],
                    smi: None,
                }
            }
            (Op::LdaSmi, &[Arg::Imm(k)]) => {
                self.acc = Acc {
                    regs: Vec::new(),
                    smi: Some(k),
                }
            }
            (Op::Star, &[Arg::Reg(r)]) => self.acc.regs.push(r),
            (Op::Mov, &[Arg::Reg(a), Arg::Reg(b)]) => {
                self.acc.regs.retain(|&x| x != b);
                if self.acc.regs.contains(&a) {
                    self.acc.regs.push(b);
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
        if !self.acc.regs.contains(&r) {
            self.emit(Op::Ldar, &[Arg::Reg(r)]);
        }
    }

    fn star(&mut self, r: u8) {
        if !self.acc.regs.contains(&r) {
            // `r` changes: the accumulator holds its new value.
            self.acc.regs.retain(|&x| x != r);
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
        } else if self.acc.regs.contains(&from) {
            self.star(to);
        } else {
            self.emit(Op::Mov, &[Arg::Reg(from), Arg::Reg(to)]);
        }
    }

    fn ic(&mut self, name: &str) -> Arg {
        self.ics.push(name.to_string());
        Arg::Index(self.ics.len() as u16 - 1)
    }

    /// A slot for an instruction with an implicit inline cache.
    fn auto_ic(&mut self) -> Arg {
        self.ic("[]")
    }

    fn global(&mut self, op: Op, name: &str) {
        let ic = self.ic(name);
        self.emit(op, &[ic]);
    }

    fn named(&mut self, op: Op, obj: u8, name: &str) {
        let ic = self.ic(name);
        self.emit(op, &[Arg::Reg(obj), ic]);
    }

    /// `LdaConstant` of `c`, one slot per distinct constant.
    fn constant(&mut self, c: Constant) {
        let same = |x: &Constant| match (x, &c) {
            (Constant::Number(a), Constant::Number(b)) => a.to_bits() == b.to_bits(),
            (a, b) => a == b,
        };
        let i = self.constants.iter().position(same).unwrap_or_else(|| {
            self.constants.push(c);
            self.constants.len() - 1
        });
        self.emit(Op::LdaConstant, &[Arg::Index(i as u16)]);
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
            ExprKind::Ident(n) => self.locals.get(n.as_str()).copied(),
            _ => None,
        }
    }

    /// Store the accumulator in variable `name`.
    fn store_var(&mut self, name: &str, pos: Pos) -> R<()> {
        match self.resolve(name, pos)? {
            Var::Reg(r) => self.star(r),
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
                if let Some(&r) = self.locals.get(n.as_str())
                    && !later.iter().any(|l| assigns(l, n))
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
            ExprKind::Update(t, d, _) => self.update(t, *d, e.pos),
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
            Str(s) => self.constant(Constant::String(s.clone())),
            Bool(true) => self.emit(Op::LdaTrue, &[]),
            Bool(false) => self.emit(Op::LdaFalse, &[]),
            Null => self.emit(Op::LdaNull, &[]),
            This => self.ldar(0),
            Ident(n) => match self.resolve(n, e.pos)? {
                Var::Reg(r) => self.ldar(r),
                Var::Undefined => self.emit(Op::LdaUndefined, &[]),
                Var::Global => self.global(Op::LdaGlobal, n),
            },
            Array(elems) => {
                self.emit(Op::CreateArray, &[]);
                if !elems.is_empty() {
                    let a = self.alloc(e.pos)?;
                    self.star(a);
                    let k = self.alloc(e.pos)?;
                    for (i, el) in elems.iter().enumerate() {
                        self.lda_smi(i as i32);
                        self.star(k);
                        self.value(el)?;
                        self.emit(Op::SetKeyed, &[Arg::Reg(a), Arg::Reg(k)]);
                    }
                    self.ldar(a);
                }
            }
            // A plain object with room for the properties, then each one
            // stored in order (a later duplicate wins).
            Object(props) => {
                // An index key is an element (`o[1]` reads it), and
                // `__proto__:` sets the prototype: neither is a named
                // property.
                if let Some((k, _)) = props.iter().find(|(k, _)| {
                    k == "__proto__" || k.parse::<u32>().is_ok_and(|i| i.to_string() == *k)
                }) {
                    return unsupported(e.pos, format!("the key `{k}` in an object literal"));
                }
                let cap = props.len().min(crate::vm::MAX_INLINE_CAPACITY as usize) as u8;
                let ic = self.auto_ic();
                self.emit(Op::CreateObject, &[Arg::Count(cap), ic]);
                if !props.is_empty() {
                    let o = self.alloc(e.pos)?;
                    self.star(o);
                    for (name, v) in props {
                        self.value(v)?;
                        self.named(Op::SetNamed, o, name);
                    }
                    self.ldar(o);
                }
            }
            Function(i) => self.closure(*i),
            Member(o, name) => {
                let r = self.reg(o, &[])?;
                self.named(Op::GetNamed, r, name);
            }
            Index(o, k) => {
                let r = self.reg(o, &[k])?;
                self.value(k)?;
                let ic = self.auto_ic();
                self.emit(Op::GetKeyed, &[Arg::Reg(r), ic]);
            }
            Call(f, args) => self.call(f, args, false, e.pos)?,
            New(f, args) => self.call(f, args, true, e.pos)?,
            Unary(UnOp::Not, _) => self.boolean(e)?,
            Unary(UnOp::Void, x) => {
                self.effect(x)?;
                self.emit(Op::LdaUndefined, &[]);
            }
            Unary(UnOp::Typeof, x) => {
                // An undeclared global is "undefined", not a ReferenceError.
                match &x.kind {
                    Ident(n) if matches!(self.resolve(n, x.pos)?, Var::Global) => {
                        self.global(Op::LdaGlobalInsideTypeof, n)
                    }
                    _ => self.value(x)?,
                }
                self.emit(Op::TypeOf, &[]);
            }
            Unary(op, _) => return unsupported(e.pos, format!("unary {op:?}")),
            Update(t, d, true) => self.update(t, *d, e.pos)?,
            Update(..) => {
                return unsupported(e.pos, "the value of a postfix increment (needs ToNumeric)");
            }
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
    fn nullish_test<'e>(op: BinOp, l: &'e Expr, r: &'e Expr) -> Option<(&'e Expr, Op)> {
        let constant = |e: &Expr| match &e.kind {
            ExprKind::Null => Some(Op::TestNull),
            // `undefined` names the global (it can't be reassigned, but a
            // local may shadow it).
            ExprKind::Ident(n) if n == "undefined" => Some(Op::TestUndefined),
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
            ExprKind::Ident(n) => {
                match (op, self.resolve(n, t.pos)?) {
                    (None, _) => self.value(v)?,
                    // `x op= v` with `x` in a register: `v`, then `Op r_x`.
                    (Some(op), Var::Reg(r))
                        if smi(v).and_then(|k| Self::smi_form(op, k)).is_none()
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
                        let ic = self.auto_ic();
                        self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                        self.apply(op, v, pos)?;
                    }
                }
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk)]);
                Ok(())
            }
            _ => unreachable!("the parser checks assignment targets"),
        }
    }

    /// `++t` or `--t` (`d` = ±1), leaving the new value in the accumulator.
    fn update(&mut self, t: &Expr, d: i8, pos: Pos) -> R<()> {
        let mark = self.temp;
        let step = |g: &mut Self| {
            if d > 0 {
                g.emit(Op::Inc, &[]);
            } else {
                g.emit(Op::SubSmi, &[Arg::Imm(1)]);
            }
        };
        match &t.kind {
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
                let ic = self.auto_ic();
                self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                step(self);
                self.emit(Op::SetKeyed, &[Arg::Reg(ro), Arg::Reg(rk)]);
            }
            _ => unreachable!("the parser checks increment targets"),
        }
        self.temp = mark;
        Ok(())
    }

    /// A call, or with `new`, a construction. The callee goes in a
    /// temporary, the receiver (for a method call) and the arguments in
    /// consecutive ones after it; a single argument already in a register
    /// is passed in place.
    fn call(&mut self, f: &Expr, args: &[Expr], new: bool, pos: Pos) -> R<()> {
        let n =
            u8::try_from(args.len()).or_else(|_| unsupported(pos, "more than 255 arguments"))?;
        let method = !new && matches!(f.kind, ExprKind::Member(..) | ExprKind::Index(..));
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
                    let ic = self.auto_ic();
                    self.emit(Op::GetKeyed, &[Arg::Reg(ro), ic]);
                }
            }
            self.star(c);
            self.mov(ro, w);
            for _ in 0..n {
                self.alloc(pos)?;
            }
            for (i, a) in args.iter().enumerate() {
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
            (true, _) => Op::Construct,
            (false, true) => Op::Call,
            (false, false) => Op::CallUndefined,
        };
        let ic = self.auto_ic();
        self.emit(op, &[Arg::Reg(c), Arg::Reg(w), Arg::Count(n), ic]);
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
        let mark = self.temp;
        let labels = std::mem::take(&mut self.pending_labels);
        match &s.kind {
            StmtKind::Var(ds) => self.var_decls(ds)?,
            StmtKind::Function(_) | StmtKind::Empty => {}
            StmtKind::Expr(e) => self.effect(e)?,
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
                match init {
                    Some(ForInit::Var(ds)) => self.var_decls(ds)?,
                    Some(ForInit::Expr(e)) => self.effect(e)?,
                    None => {}
                }
                let first_passes = match test {
                    None => true,
                    Some(c) => first_test_passes(init.as_ref(), c),
                };
                self.looping(labels, test.as_ref(), update.as_ref(), body, !first_passes)?;
            }
            StmtKind::While(c, body) => {
                let first_passes = matches!(c.kind, ExprKind::Bool(true));
                self.looping(labels, Some(c), None, body, !first_passes)?;
            }
            StmtKind::DoWhile(body, c) => self.looping(labels, Some(c), None, body, false)?,
            StmtKind::ForIn {
                name, object, body, ..
            } => self.for_in(labels, name, object, body, s.pos)?,
            StmtKind::Switch(d, cases) => self.switch(labels, d, cases)?,
            StmtKind::Try(_, _, Some(_)) => return unsupported(s.pos, "`finally`"),
            StmtKind::Try(block, Some((name, body)), None) => {
                // The block, then past the handler. An exception thrown in
                // the block's range lands in the handler with its value in
                // `acc`, which goes to the catch parameter's own register.
                let start = self.asm.offset() as u32;
                self.in_try += 1;
                for s in block {
                    self.stmt(s)?;
                }
                self.in_try -= 1;
                let end = self.asm.offset() as u32;
                let after = self.label();
                if self.reachable {
                    self.jump(after);
                }
                let handler = self.label();
                self.labels[handler.0].used = true;
                self.reachable = false;
                self.bind(handler);
                self.handlers.push((start, end, self.asm.offset() as u32));
                let r = self.alloc(s.pos)?;
                self.star(r);
                let outer = self.locals.insert(name.as_str(), r);
                for s in body {
                    self.stmt(s)?;
                }
                match outer {
                    Some(o) => self.locals.insert(name.as_str(), o),
                    None => self.locals.remove(name.as_str()),
                };
                self.bind(after);
            }
            StmtKind::Try(..) => unreachable!("the parser requires `catch` or `finally`"),
            StmtKind::Throw(e) => {
                self.value(e)?;
                self.emit(Op::Throw, &[]);
            }
            StmtKind::Return(e) => {
                match e {
                    Some(e) => self.value(e)?,
                    None => self.emit(Op::LdaUndefined, &[]),
                }
                self.emit(Op::Return, &[]);
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
                self.jump(l);
            }
            StmtKind::Block(b) => {
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
        labels: Vec<String>,
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
        labels: Vec<String>,
        name: &str,
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
        let ic = self.auto_ic();
        self.emit(Op::GetKeyed, &[Arg::Reg(keys), ic]);
        self.store_var(name, pos)?;
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

    fn var_decls(&mut self, ds: &[(String, Option<Expr>)]) -> R<()> {
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
        labels: Vec<String>,
        test: Option<&Expr>,
        update: Option<&Expr>,
        body: &'a Stmt,
        jump_to_test: bool,
    ) -> R<()> {
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
        Some(ForInit::Var(ds)) => match ds.last() {
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
