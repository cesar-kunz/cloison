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
    pub ics: Vec<String>,
    /// Module function index of each nested function (`CreateClosure`'s
    /// operand indexes this).
    pub closures: Vec<usize>,
}

#[derive(Clone, Debug)]
pub struct Module {
    pub functions: Vec<FunctionDef>,
    /// Index of the top-level code in `functions`.
    pub script: usize,
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
        let line = raw.split(';').next().unwrap().trim();
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
        let mut ics: Vec<String> = Vec::new();
        let mut closures: Vec<usize> = Vec::new();
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
            let toks: Vec<&str> = operands
                .split(',')
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .collect();
            let kinds = op.operands();
            if toks.len() != kinds.len() {
                return Err(err(format!("{mnemonic} takes {} operands", kinds.len())));
            }
            let mut args = Vec::new();
            for (&k, &t) in kinds.iter().zip(&toks) {
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
                        ics.push(t.to_string());
                        Arg::Index(
                            u16::try_from(ics.len() - 1)
                                .map_err(|_| err("too many inline caches".into()))?,
                        )
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
            if let [_, Arg::Reg(w), Arg::Count(c)] = args[..] {
                let extra = usize::from(op == Op::Call);
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
        });
    }
    Ok(Module { functions, script })
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
        assert_eq!(f.ics, ["x"]);
        assert_eq!(m.functions[m.script].closures, [1]);
        assert_eq!(f.code[6], Op::JumpIfLessSmiLoop as u8);
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
