//! The text assembly format for bytecode modules, and the module itself.
//!
//! A module is what a JS front end produces from a script: its functions'
//! bytecode, with the names each inline-cache slot looks up and the nested
//! functions each one creates. It is independent of any VM; [`crate::vm::Vm`]
//! loads it, interning names. A binary encoding of the same structure will
//! be the bytecode compiler's output format.
//!
//! The text format (see `bench/bytecode/` for examples):
//!
//! ```text
//! ; comment
//! script                          ; the top-level code
//!         CreateClosure f
//!         StaGlobal f             ; the operand names the property
//!         …
//! end
//!
//! function f(a, b)                ; parameter names only document; r1 = a, r2 = b
//! loop:   Ldar r1                 ; a label, then an instruction
//!         JumpIfLessSmi 10, loop  ; backward jumps get the loop form
//!         Return
//! end
//! ```
//!
//! Registers are `r0` (`this`), then the parameters, then locals and
//! temporaries; a function's register count is the highest one it uses,
//! plus one. Operands are separated by commas: registers, integers, labels,
//! property names (for inline-cache slots) and function names.

use std::collections::HashMap;

use crate::bytecode::{Arg, Assembler, Label, Op, Operand};

/// One function's bytecode and its tables.
#[derive(Clone, Debug)]
pub struct FunctionDef {
    pub name: String,
    pub nparams: u32,
    /// Register count, `r0` (`this`) and the parameters included.
    pub nregs: u32,
    pub code: Vec<u8>,
    /// The property name of each inline-cache slot.
    pub ics: Vec<std::rc::Rc<str>>,
    /// The `SetNamed` slots of object literals' properties, which define
    /// them (see `vm::Code::define_ics`).
    pub defines: Vec<u16>,
    /// Module function index of each nested function (`CreateClosure`'s
    /// operand indexes this).
    pub closures: Vec<usize>,
    /// The constants `LdaConstant` loads.
    pub constants: Vec<Constant>,
    /// Exception handlers: an exception at a bytecode offset in
    /// `start..end` continues at `handler` with its value in `acc` (the
    /// first that covers it: inner `try`s come first).
    pub handlers: Vec<(u32, u32, u32)>,
    /// The `GetKeyed` and the `SetKeyed` of each `o[k] op= v`, `o[k]++` or
    /// `o[k] ??= v` (bytecode offsets): an object key is converted once
    /// for both (see `Vm::key_cache`).
    pub compound_keys: Vec<(u32, u32)>,
    /// Where each upvalue `CreateClosure` gives the function comes from in
    /// the function creating it: a register (`true`) or an upvalue of its
    /// own (see `vm::Upvalue`).
    pub upvalues: Vec<(bool, u8)>,
    /// Nested functions use some of its variables (they stay in its
    /// registers, which its closures' upvalues point to until it returns).
    pub captures: bool,
    /// Strict mode code: `this` is passed as is (see `ast::Function::strict`).
    pub strict: bool,
    /// Where it starts in its source (its parameter list's `(`), 1-based
    /// line and column, for `CLOISON_JIT_LOG` and `CLOISON_JIT_DUMP_FN`: (0, 0)
    /// if unknown or neither is set (see `codegen::positions`).
    pub line: u32,
    pub col: u32,
    /// Not compiled yet: compiled from its source at its first call (its
    /// `code` the one `CompileLazy`; `name`, `nparams` and `upvalues` as
    /// compiled).
    pub lazy: Option<LazyFn>,
    /// What it is (see `vm::Code::kind`).
    pub kind: u8,
    /// Its `length` property.
    pub length: u32,
    /// Its source text: the source and the span in it, for
    /// `Function.prototype.toString`.
    pub text: Option<(std::rc::Rc<str>, u32, u32)>,
}

/// A function to compile when it is first called: its text in the source
/// it came from, and its upvalues' names, in order (see
/// `js::compile_lazy`).
#[derive(Clone, Debug)]
pub struct LazyFn {
    pub source: std::rc::Rc<str>,
    /// The parameter list's `(`, and past the body's `}`.
    pub start: u32,
    pub end: u32,
    /// A named function expression's name, bound in its body.
    pub own_name: Option<std::rc::Rc<str>>,
    pub upvalues: Vec<std::rc::Rc<str>>,
    /// Those of `upvalues` that are an enclosing named function
    /// expression's own name: immutable (see `codegen::own_name_binding`).
    pub own_names: Vec<std::rc::Rc<str>>,
    /// Nested in strict code.
    pub strict: bool,
}

/// A constant: a string or a number (one that isn't an int32).
#[derive(Clone, Debug, PartialEq)]
pub enum Constant {
    Number(f64),
    String(String),
}

impl Constant {
    /// As the text format writes it: a quoted string, or a number Rust
    /// reads back exactly.
    fn text(&self) -> String {
        match self {
            Constant::Number(d) => format!("{d:?}"),
            Constant::String(s) => format!("{s:?}"),
        }
    }

    /// The constant written as `t`, if it is one.
    fn parse(t: &str) -> Option<Constant> {
        if let Some(inner) = t.strip_prefix('"').and_then(|r| r.strip_suffix('"')) {
            let mut out = String::new();
            let mut cs = inner.chars();
            while let Some(c) = cs.next() {
                if c != '\\' {
                    out.push(c);
                    continue;
                }
                match cs.next()? {
                    'n' => out.push('\n'),
                    't' => out.push('\t'),
                    'r' => out.push('\r'),
                    '0' => out.push('\0'),
                    'u' => {
                        // `\u{…}`, as Rust's `{:?}` writes.
                        let rest: String = cs.by_ref().take_while(|&c| c != '}').collect();
                        let hex = rest.strip_prefix('{')?;
                        out.push(char::from_u32(u32::from_str_radix(hex, 16).ok()?)?);
                    }
                    c => out.push(c),
                }
            }
            return Some(Constant::String(out));
        }
        let d = match t {
            "inf" => f64::INFINITY,
            "-inf" => f64::NEG_INFINITY,
            "NaN" => f64::NAN,
            _ => t.parse().ok()?,
        };
        Some(Constant::Number(d))
    }
}

/// `line` up to a `;` comment, but not one in a string.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    let mut escaped = false;
    for (i, c) in line.char_indices() {
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ';' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

#[derive(Clone, Debug)]
pub struct Module {
    pub functions: Vec<FunctionDef>,
    /// Index of the top-level code in `functions`.
    pub script: usize,
    /// The script's `var` and function names: globals made (`undefined`)
    /// before it runs (see `Vm::global_declarations`).
    pub declared: Vec<String>,
    /// Which of `declared` are functions' (declarations at its top level).
    pub function_names: Vec<String>,
    /// Its own block-scoped names (`let`, `const`, `class`): a script's
    /// may not be declared by another script (see `Vm::declare_script`).
    pub lexical: Vec<String>,
}

/// Assemble a module from its text form.
pub fn parse(src: &str) -> Result<Module, String> {
    // Pass 1: find the blocks, so `CreateClosure` can name functions
    // defined further down.
    struct Block<'a> {
        name: String,
        params: u32,
        lines: Vec<(usize, &'a str)>,
    }
    let mut blocks: Vec<Block> = Vec::new();
    let mut open: Option<Block> = None;
    for (n, raw) in src.lines().enumerate() {
        let n = n + 1;
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        match &mut open {
            None => {
                let (name, params) = if line == "script" {
                    ("<script>".to_string(), 0)
                } else if let Some(rest) = line.strip_prefix("function ") {
                    parse_signature(rest).ok_or(format!("line {n}: bad function header"))?
                } else {
                    return Err(format!("line {n}: expected `script` or `function`"));
                };
                if blocks.iter().any(|b| b.name == name) {
                    return Err(format!("line {n}: {name} defined twice"));
                }
                open = Some(Block {
                    name,
                    params,
                    lines: Vec::new(),
                });
            }
            Some(b) if line == "end" => {
                let _ = b;
                blocks.push(open.take().unwrap());
            }
            Some(b) => b.lines.push((n, line)),
        }
    }
    if let Some(b) = open {
        return Err(format!("{} has no `end`", b.name));
    }
    let index: HashMap<&str, usize> = blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.name.as_str(), i))
        .collect();
    let script = *index.get("<script>").ok_or("no `script` block")?;

    // Pass 2: assemble each block.
    let mut functions = Vec::new();
    for b in &blocks {
        let mut a = Assembler::new();
        let mut labels: HashMap<String, (Label, bool)> = HashMap::new();
        let mut ics: Vec<std::rc::Rc<str>> = Vec::new();
        let mut closures: Vec<usize> = Vec::new();
        let mut constants: Vec<Constant> = Vec::new();
        let mut nregs = 1 + b.params as usize;
        fn label(
            labels: &mut HashMap<String, (Label, bool)>,
            a: &mut Assembler,
            name: &str,
        ) -> Label {
            labels
                .entry(name.to_string())
                .or_insert_with(|| (a.label(), false))
                .0
        }
        for &(n, line) in &b.lines {
            let err = |m: String| format!("line {n}: {m}");
            let mut rest = line;
            if let Some((l, after)) = line.split_once(':') {
                let l = l.trim();
                if !is_ident(l) {
                    return Err(err(format!("bad label `{l}`")));
                }
                let lab = label(&mut labels, &mut a, l);
                let bound = &mut labels.get_mut(l).unwrap().1;
                if *bound {
                    return Err(err(format!("label `{l}` bound twice")));
                }
                *bound = true;
                a.bind(lab);
                rest = after.trim();
                if rest.is_empty() {
                    continue;
                }
            }
            let (mnemonic, operands) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            let op = Op::from_name(mnemonic)
                .ok_or_else(|| err(format!("unknown opcode `{mnemonic}`")))?;
            // (A function read here has no upvalues: one indexed would be
            // past the closure's.)
            if matches!(op, Op::LdaUpvalue | Op::StaUpvalue) {
                return Err(err(format!("{mnemonic}: the text form has no upvalues")));
            }
            // A constant is the only operand, and may contain commas.
            let toks: Vec<&str> = if op.operands() == [Operand::Const] {
                vec![operands.trim()]
            } else {
                operands
                    .split(',')
                    .map(str::trim)
                    .filter(|t| !t.is_empty())
                    .collect()
            };
            let kinds = op.operands();
            let written = kinds.iter().filter(|&&k| k != Operand::AutoIc).count();
            if toks.len() != written {
                return Err(err(format!("{mnemonic} takes {written} operands")));
            }
            let mut toks = toks.iter();
            let mut args = Vec::new();
            for &k in kinds {
                if k == Operand::AutoIc {
                    // Keyed accesses' caches are named after no property.
                    ics.push("[]".into());
                    args.push(Arg::Index(
                        u16::try_from(ics.len() - 1)
                            .map_err(|_| err("too many inline caches".into()))?,
                    ));
                    continue;
                }
                let t = *toks.next().unwrap();
                let arg = match k {
                    Operand::Reg => {
                        let r = t
                            .strip_prefix('r')
                            .and_then(|d| d.parse::<u8>().ok())
                            .ok_or_else(|| err(format!("expected a register, got `{t}`")))?;
                        nregs = nregs.max(r as usize + 1);
                        Arg::Reg(r)
                    }
                    Operand::Imm => Arg::Imm(
                        t.parse()
                            .map_err(|_| err(format!("expected an integer, got `{t}`")))?,
                    ),
                    Operand::Count => Arg::Count(
                        t.parse()
                            .map_err(|_| err(format!("expected a count, got `{t}`")))?,
                    ),
                    Operand::Off => {
                        if !is_ident(t) {
                            return Err(err(format!("expected a label, got `{t}`")));
                        }
                        Arg::Label(label(&mut labels, &mut a, t))
                    }
                    Operand::Ic => {
                        if !is_ident(t) {
                            return Err(err(format!("expected a property name, got `{t}`")));
                        }
                        ics.push(t.into());
                        Arg::Index(
                            u16::try_from(ics.len() - 1)
                                .map_err(|_| err("too many inline caches".into()))?,
                        )
                    }
                    Operand::AutoIc => unreachable!(),
                    Operand::Const => {
                        let c = Constant::parse(t)
                            .ok_or_else(|| err(format!("expected a constant, got `{t}`")))?;
                        let i = match constants.iter().position(|x| x.text() == c.text()) {
                            Some(i) => i,
                            None => {
                                constants.push(c);
                                constants.len() - 1
                            }
                        };
                        Arg::Index(u16::try_from(i).map_err(|_| err("too many constants".into()))?)
                    }
                    Operand::Fn => {
                        let f = *index
                            .get(t)
                            .ok_or_else(|| err(format!("no function `{t}`")))?;
                        let i = closures.iter().position(|&c| c == f).unwrap_or_else(|| {
                            closures.push(f);
                            closures.len() - 1
                        });
                        Arg::Index(i as u16)
                    }
                };
                args.push(arg);
            }
            // A call's argument window counts as used registers.
            if let [_, Arg::Reg(w), Arg::Count(c), ..] = args[..] {
                let extra = usize::from(matches!(op, Op::Call | Op::CallApplyArguments));
                nregs = nregs.max(w as usize + c as usize + extra);
            }
            a.emit(op, &args);
        }
        if let Some((name, _)) = labels.iter().find(|(_, (_, bound))| !bound) {
            return Err(format!("{}: label `{name}` is never bound", b.name));
        }
        if nregs > 256 {
            return Err(format!("{}: more than 256 registers", b.name));
        }
        functions.push(FunctionDef {
            name: b.name.clone(),
            nparams: b.params,
            nregs: nregs as u32,
            code: a.finish(),
            ics,
            defines: Vec::new(),
            closures,
            constants,
            handlers: Vec::new(),
            compound_keys: Vec::new(),
            upvalues: Vec::new(),
            captures: false,
            strict: false,
            line: 0,
            col: 0,
            lazy: None,
            kind: 0,
            length: b.params,
            text: None,
        });
    }
    let module = Module {
        lexical: Vec::new(),
        functions,
        script,
        declared: Vec::new(),
        function_names: Vec::new(),
    };
    verify(&module)?;
    Ok(module)
}

/// Check that `m` is bytecode the interpreter can run safely, which it
/// assumes without checking (see `interp::run_frame`): every instruction
/// a known one and complete, every path ending in a `Return`, `Throw` or
/// jump, jumps and handlers landing on instructions, registers (calls'
/// windows too) within the frame, and table indices within their tables.
/// A module from `js::compile` is right by construction; one made any
/// other way (the text format, or [`Module`] built directly) must pass
/// this before `Vm::run`.
///
/// Only bytecode whose safety this can see is accepted: not the
/// instructions whose operands must hold what the front end put there
/// (generators' `InitialYield`, `Yield` and `AsyncStart`, `PushLiteral`'s
/// array, `DefineAccessor`'s object), lazily compiled stubs, nor class
/// constructors, generators or async functions (`kind` over 1).
pub fn verify(m: &Module) -> Result<(), String> {
    let n = m.functions.len();
    if m.script >= n {
        return Err("the script is not one of the functions".into());
    }
    // How many upvalues each function's closures get.
    let upvalues: Vec<usize> = m.functions.iter().map(|f| f.upvalues.len()).collect();
    if upvalues[m.script] != 0 {
        return Err("the script has upvalues".into());
    }
    for f in &m.functions {
        let err = |at: usize, what: String| Err(format!("{}: at {at}: {what}", f.name));
        if f.lazy.is_some() || f.kind > 1 {
            return Err(format!("{}: a stub, class constructor, generator or async function", f.name));
        }
        if f.nregs > 256 || f.nregs < f.nparams + 1 {
            return Err(format!("{}: {} registers for {} parameters", f.name, f.nregs, f.nparams));
        }
        if let Some(&c) = f.closures.iter().find(|&&c| c >= n) {
            return Err(format!("{}: nested function {c} is not one of the module's", f.name));
        }
        // Where a closure's upvalues come from: here.
        for &c in &f.closures {
            for &(local, k) in &m.functions[c].upvalues {
                if local && k as u32 >= f.nregs || !local && k as usize >= f.upvalues.len() {
                    return Err(format!("{}: an upvalue of {} out of range", f.name, m.functions[c].name));
                }
            }
        }
        let code = &f.code;
        // Instruction boundaries.
        let mut starts = vec![false; code.len() + 1];
        let mut at = 0;
        let mut last = None;
        while at < code.len() {
            starts[at] = true;
            let Some(op) = Op::from_u8(code[at]) else {
                return err(at, format!("opcode {} unknown", code[at]));
            };
            if at + op.size() > code.len() {
                return err(at, format!("{op:?} incomplete"));
            }
            last = Some(op);
            at += op.size();
        }
        let Some(last) = last else {
            return Err(format!("{}: no instructions", f.name));
        };
        if !matches!(last, Op::Return | Op::Throw | Op::Jump | Op::JumpLoop) {
            return Err(format!("{}: its last instruction ({last:?}) falls off its end", f.name));
        }
        let mut at = 0;
        while at < code.len() {
            let op = Op::from_u8(code[at]).unwrap();
            let mut regs: Vec<usize> = Vec::new();
            let mut operand = at + 1;
            let mut vals: Vec<i64> = Vec::new();
            for &k in op.operands() {
                let b = &code[operand..operand + k.width()];
                let v = match k.width() {
                    1 => b[0] as i64,
                    2 => u16::from_le_bytes([b[0], b[1]]) as i64,
                    _ => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as i64,
                };
                vals.push(v);
                match k {
                    Operand::Reg => regs.push(v as usize),
                    Operand::Off => {
                        let t = at as i64 + v;
                        if t < 0 || t as usize >= code.len() || !starts[t as usize] {
                            return err(at, format!("{op:?} jumps to {t}, not an instruction"));
                        }
                    }
                    Operand::Ic | Operand::AutoIc if v as usize >= f.ics.len() => {
                        return err(at, format!("{op:?}: cache slot {v} out of range"));
                    }
                    Operand::Fn if v as usize >= f.closures.len() => {
                        return err(at, format!("{op:?}: nested function {v} out of range"));
                    }
                    Operand::Const if v as usize >= f.constants.len() => {
                        return err(at, format!("{op:?}: constant {v} out of range"));
                    }
                    _ => {}
                }
                operand += k.width();
            }
            // Registers past the operands': calls' argument windows.
            let end = match op {
                Op::Call => Some(vals[1] + vals[2] + 1),
                Op::CallUndefined | Op::Construct => Some(vals[1] + vals[2]),
                Op::CallApplyArguments => Some(vals[1] + 3),
                Op::Intrinsic => Some(vals[1] + vals[2]),
                _ => None,
            };
            if regs.iter().any(|&r| r >= f.nregs as usize) || end.is_some_and(|e| e > f.nregs as i64) {
                return err(at, format!("{op:?}: a register out of the frame's {}", f.nregs));
            }
            let string = |k: i64| matches!(f.constants[k as usize], Constant::String(_));
            match op {
                Op::CompileLazy | Op::InitialYield | Op::Yield | Op::AsyncStart | Op::PushLiteral | Op::DefineAccessor => {
                    return err(at, format!("{op:?} is the front end's only"));
                }
                Op::LdaUpvalue | Op::StaUpvalue if vals[0] as usize >= f.upvalues.len() => {
                    return err(at, format!("{op:?}: upvalue {} out of range", vals[0]));
                }
                Op::Intrinsic if vals[0] as usize >= crate::intrinsics::INTRINSICS.len() => {
                    return err(at, format!("intrinsic {} unknown", vals[0]));
                }
                Op::CreateRegExp if !string(vals[0]) || !string(vals[1]) => {
                    return err(at, "CreateRegExp of a constant that isn't a string".into());
                }
                Op::CheckHole if !string(vals[0]) => {
                    return err(at, "CheckHole of a constant that isn't a string".into());
                }
                _ => {}
            }
            // A fused comparison reads the jump after it (see `quicken`).
            if op.plain() != op && !op.is_loop() {
                let next = at + op.size();
                if !matches!(code.get(next).and_then(|&b| Op::from_u8(b)), Some(Op::JumpIfTrue | Op::JumpIfFalse)) {
                    return err(at, format!("{op:?} not followed by its jump"));
                }
            }
            at += op.size();
        }
        let inside = |o: u32| (o as usize) < code.len() && starts[o as usize];
        for &(start, end, handler) in &f.handlers {
            if start > end || end as usize > code.len() || !inside(handler) {
                return Err(format!("{}: a handler out of range", f.name));
            }
        }
        if f.compound_keys.iter().any(|&(g, s)| !inside(g) || !inside(s)) {
            return Err(format!("{}: a compound key out of range", f.name));
        }
        if f.defines.iter().any(|&d| d as usize >= f.ics.len()) {
            return Err(format!("{}: a define slot out of range", f.name));
        }
    }
    Ok(())
}

/// The text form of a module, which [`parse`] reads back into the same
/// module. Jump targets get labels `L1`, `L2`, … in address order,
/// parameters are named `p1`, `p2`, …, and functions keep their names,
/// numbered (`f_2`) where two share one.
pub fn disassemble(m: &Module) -> String {
    use std::fmt::Write;
    let mut names: Vec<String> = Vec::new();
    for (i, f) in m.functions.iter().enumerate() {
        let base = if i == m.script {
            "script".to_string()
        } else if is_ident(&f.name) && f.name != "script" {
            f.name.clone()
        } else {
            "anonymous".to_string()
        };
        let mut name = base.clone();
        let mut n = 1;
        while names.contains(&name) {
            n += 1;
            name = format!("{base}_{n}");
        }
        names.push(name);
    }
    let mut out = String::new();
    for (i, f) in m.functions.iter().enumerate() {
        if i == m.script {
            out.push_str("script\n");
        } else {
            let params: Vec<String> = (1..=f.nparams).map(|p| format!("p{p}")).collect();
            let _ = writeln!(out, "function {}({})", names[i], params.join(", "));
        }
        // (Not read back: the text form has no handlers yet.)
        for &(start, end, handler) in &f.handlers {
            let _ = writeln!(out, "        ; handler: {start}..{end} -> {handler}");
        }
        let code = &f.code;
        let decode = |pc: usize| {
            let op = Op::from_u8(code[pc]).expect("valid bytecode");
            (op, pc + op.size())
        };
        // Label the jump targets.
        let mut targets = Vec::new();
        let mut pc = 0;
        while pc < code.len() {
            let (op, next) = decode(pc);
            let mut at = pc + 1;
            for &k in op.operands() {
                if k == Operand::Off {
                    let off = i32::from_le_bytes(code[at..at + 4].try_into().unwrap());
                    targets.push((pc as i64 + off as i64) as usize);
                }
                at += k.width();
            }
            pc = next;
        }
        targets.sort_unstable();
        targets.dedup();
        let label = |t: usize| format!("L{}", targets.binary_search(&t).unwrap() + 1);
        let mut pc = 0;
        while pc < code.len() {
            let (op, next) = decode(pc);
            let head = if targets.binary_search(&pc).is_ok() {
                format!("{}:", label(pc))
            } else {
                String::new()
            };
            let mut args = Vec::new();
            let mut at = pc + 1;
            for &k in op.operands() {
                let u16_at = || u16::from_le_bytes([code[at], code[at + 1]]) as usize;
                match k {
                    Operand::Reg => args.push(format!("r{}", code[at])),
                    Operand::Count => args.push(code[at].to_string()),
                    Operand::Imm => args
                        .push(i32::from_le_bytes(code[at..at + 4].try_into().unwrap()).to_string()),
                    Operand::Off => {
                        let off = i32::from_le_bytes(code[at..at + 4].try_into().unwrap());
                        args.push(label((pc as i64 + off as i64) as usize));
                    }
                    Operand::Ic => args.push(f.ics[u16_at()].to_string()),
                    Operand::Fn => args.push(names[f.closures[u16_at()]].clone()),
                    Operand::Const => args.push(f.constants[u16_at()].text()),
                    Operand::AutoIc => {}
                }
                at += k.width();
            }
            let _ = writeln!(
                out,
                "{head:<8}{}{}{}",
                op.plain().name(),
                if args.is_empty() { "" } else { " " },
                args.join(", ")
            );
            pc = next;
        }
        out.push_str("end\n\n");
    }
    out
}

/// `name(a, b)` → `(name, 2)`.
fn parse_signature(s: &str) -> Option<(String, u32)> {
    let (name, rest) = s.split_once('(')?;
    let params = rest.trim().strip_suffix(')')?;
    let name = name.trim();
    if !is_ident(name) {
        return None;
    }
    let names: Vec<&str> = params
        .split(',')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .collect();
    if !names.iter().all(|p| is_ident(p)) {
        return None;
    }
    Some((name.to_string(), names.len() as u32))
}

fn is_ident(s: &str) -> bool {
    let mut cs = s.chars();
    matches!(cs.next(), Some(c) if c.is_ascii_alphabetic() || c == '_' || c == '$')
        && cs.all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_functions_labels_and_tables() {
        let m = parse(
            "script\n  CreateClosure f\n  StaGlobal f\n  Return\nend\n\
             function f(a, b) ; doc\nl: Ldar r2\n  GetNamed r1, x\n  JumpIfLessSmi 3, l\n  CallUndefined r1, r5, 2\n  Return\nend\n",
        )
        .unwrap();
        let f = &m.functions[1];
        assert_eq!((f.name.as_str(), f.nparams, f.nregs), ("f", 2, 7));
        // The call has an implicit cache slot.
        assert_eq!(f.ics, ["x".into(), "[]".into()]);
        assert_eq!(m.functions[m.script].closures, [1]);
        assert_eq!(f.code[6], Op::JumpIfLessSmiLoop as u8);
    }

    #[test]
    fn disassembly_reads_back_as_the_same_module() {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/bytecode");
        for e in std::fs::read_dir(dir).unwrap() {
            let path = e.unwrap().path();
            if path.extension().is_none_or(|x| x != "asm") {
                continue;
            }
            let m = parse(&std::fs::read_to_string(&path).unwrap()).unwrap();
            let text = disassemble(&m);
            let back = parse(&text).unwrap_or_else(|e| panic!("{}: {e}\n{text}", path.display()));
            assert_eq!(text, disassemble(&back), "{}", path.display());
            for (a, b) in m.functions.iter().zip(&back.functions) {
                assert_eq!(
                    (&a.code, &a.ics, &a.closures, a.nparams, a.nregs),
                    (&b.code, &b.ics, &b.closures, b.nparams, b.nregs),
                    "{}: {}",
                    path.display(),
                    a.name
                );
            }
        }
    }

    #[test]
    fn constants_read_back() {
        let m = parse(
            "script\n  LdaConstant \"a, b; \\\"c\\\" \\u{e9}\"\n  LdaConstant 2.5\n  LdaConstant -0.0 ; neg\n  LdaConstant 2.5\n  Return\nend\n",
        )
        .unwrap();
        let f = &m.functions[0];
        assert_eq!(
            f.constants,
            [
                Constant::String("a, b; \"c\" é".into()),
                Constant::Number(2.5),
                Constant::Number(-0.0),
            ]
        );
        let back = parse(&disassemble(&m)).unwrap();
        assert_eq!(back.functions[0].constants, f.constants);
        assert_eq!(back.functions[0].code, f.code);
    }

    #[test]
    fn reports_errors_with_lines() {
        let e = parse("script\n  Bogus r1\nend\n").unwrap_err();
        assert!(e.starts_with("line 2:"), "{e}");
        let e = parse("script\n  Jump nowhere\nend\n").unwrap_err();
        assert!(e.contains("never bound"), "{e}");
        let e = parse("script\n  Ldar 3\nend\n").unwrap_err();
        assert!(e.contains("register"), "{e}");
        let e = parse("script\n  LdaUpvalue 0\n  Return\nend\n").unwrap_err();
        assert!(e.contains("no upvalues"), "{e}");
    }

    /// What [`verify`] rejects: code the interpreter would run off, out
    /// of its frame or its tables with.
    #[test]
    fn verify_rejects_unsafe_bytecode() {
        let ok = parse("script\n  LdaSmi 1\n  Return\nend\n").unwrap();
        let bad = |edit: &dyn Fn(&mut FunctionDef)| {
            let mut m = ok.clone();
            edit(&mut m.functions[0]);
            verify(&m).unwrap_err()
        };
        assert!(parse("script\nend\n").unwrap_err().contains("no instructions"));
        assert!(parse("script\n  LdaSmi 1\nend\n").unwrap_err().contains("falls off"));
        assert!(bad(&|f| f.code.truncate(3)).contains("incomplete"));
        assert!(bad(&|f| f.code[0] = 255).contains("unknown"));
        // `Jump` into the middle of `LdaSmi`.
        assert!(bad(&|f| f.code = vec![Op::Jump as u8, 2, 0, 0, 0, Op::Return as u8]).contains("not an instruction"));
        assert!(bad(&|f| f.code = vec![Op::Ldar as u8, 9, Op::Return as u8]).contains("register"));
        // `Call r0, r0, 5`'s window past the frame.
        assert!(bad(&|f| {
            f.code = vec![Op::Call as u8, 0, 0, 5, 0, 0, Op::Return as u8];
            f.ics.push("".into());
        })
        .contains("register"));
        assert!(bad(&|f| f.code = vec![Op::LdaConstant as u8, 0, 0, Op::Return as u8]).contains("constant"));
        assert!(bad(&|f| f.code = vec![Op::TestNullJumpIfTrue as u8, Op::Return as u8]).contains("its jump"));
        assert!(bad(&|f| f.kind = 4).contains("generator"));
        assert!(bad(&|f| f.handlers.push((0, 6, 3))).contains("handler"));
    }
}
