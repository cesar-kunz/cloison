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
    /// Module function index of each nested function (`CreateClosure`'s
    /// operand indexes this).
    pub closures: Vec<usize>,
    /// The constants `LdaConstant` loads.
    pub constants: Vec<Constant>,
    /// Exception handlers: an exception at a bytecode offset in
    /// `start..end` continues at `handler` with its value in `acc` (the
    /// first that covers it: inner `try`s come first).
    pub handlers: Vec<(u32, u32, u32)>,
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
    /// line and column, for `TOI_JIT_LOG` and `TOI_JIT_DUMP_FN`: (0, 0)
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
    /// The script's `var` and function names: globals that exist (as
    /// `undefined`) before it assigns them (see `Vm::get_global_miss`).
    pub declared: Vec<String>,
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
            closures,
            constants,
            handlers: Vec::new(),
            upvalues: Vec::new(),
            captures: false,
            strict: false,
            line: 0,
            col: 0,
            lazy: None,
            kind: 0,
            length: b.params,
        });
    }
    Ok(Module {
        lexical: Vec::new(),
        functions,
        script,
        declared: Vec::new(),
    })
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
    }
}
