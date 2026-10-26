//! Checks of every stencil's machine code, after `build.rs`'s rewrites,
//! of what the JIT assumes when it copies and patches it. A violation fails
//! the build: LLVM or a rewrite produced code the JIT would miscompile
//! with, silently.
//!
//! - The code decodes, exactly to its end, and doesn't fall off it.
//! - Each relocation patches one instruction's field: a branch's to a
//!   continuation, an exit, a helper or a runtime function, its
//!   displacement at the end of a `jmp`/`jcc`/`call` (x86-64), or the
//!   instruction itself (arm64).
//! - Local branches land on an instruction of the stencil.
//! - No PC-relative reference without a relocation: the JIT copies code,
//!   not data.
//! - An exit leaves before the stencil changed anything: no memory store
//!   (but to the native stack) on a path to a branch to `cloison_exit`,
//!   except in [`STORES_BEFORE_EXIT`].

/// A relocation as the checks see it: where its field is, its ELF type,
/// what it refers to (`@slow` for the stencil's helper).
pub struct Reloc<'a> {
    pub offset: usize,
    pub r_type: u32,
    pub symbol: &'a str,
}

/// Stencils that may store before an exit, and why that is safe.
const STORES_BEFORE_EXIT: &[(&str, &str)] = &[
    // Counting down a function's heat is the point: the exit at zero is
    // the tier-up, and the interpreter doesn't count it again.
    ("tier_count", "counts down the heat it exits on"),
    // Its exits come after the call returned, as `call`'s do: the
    // interpreter resumes past the `new`, not at it.
    ("construct_direct", "exits after its call, as `call`"),
    // Recording the exit is the point: its exit is the code's exit tail,
    // which returns to the driver, never an exit stub that resumes.
    ("exit", "records the exit, then goes to the code's exit tail"),
];

/// The violations in stencil `name` (code and relocations), on `arch`.
pub fn check(name: &str, arch: &str, code: &[u8], relocs: &[Reloc]) -> Vec<String> {
    let mut errors = Vec::new();
    let insns = match arch {
        "x86_64" => x86::decode(code),
        "aarch64" => arm64::decode(code),
        _ => return errors,
    };
    let insns = match insns {
        Ok(i) => i,
        Err(e) => return vec![e],
    };
    if insns.is_empty() {
        return vec!["no code".into()];
    }
    let starts: std::collections::HashSet<usize> = insns.iter().map(|i| i.at).collect();
    let index_of = |at: usize| insns.binary_search_by_key(&at, |i| i.at).ok();
    let containing = |off: usize| insns.iter().position(|i| (i.at..i.at + i.len).contains(&off));

    let last = insns.last().unwrap();
    if !last.ends_flow {
        errors.push(format!("falls off its end after {:#x}", last.at));
    }

    // Relocations, and which instructions branch to a symbol.
    let mut sym_branch: Vec<Option<&str>> = vec![None; insns.len()];
    for r in relocs {
        let off = r.offset;
        let Some(k) = containing(off) else {
            errors.push(format!("relocation at {off:#x} past the code"));
            continue;
        };
        let i = &insns[k];
        let width = match (arch, r.r_type) {
            ("x86_64", 1) => 8,
            ("x86_64", _) => 4,
            _ => 4,
        };
        let whole = if arch == "x86_64" { off > i.at && off + width <= i.at + i.len } else { off == i.at };
        if !whole {
            errors.push(format!("relocation to `{}` at {off:#x} isn't within one instruction's field", r.symbol));
            continue;
        }
        let branch_sym = r.symbol == "cloison_continue"
            || r.symbol == "cloison_jump_target"
            || r.symbol == "cloison_exit"
            || r.symbol == "@slow"
            || r.symbol.starts_with("cloison_rt_");
        if branch_sym {
            let field_ok = arch != "x86_64" || off + 4 == i.at + i.len;
            if !i.branch || !field_ok {
                errors.push(format!("relocation to `{}` at {off:#x} isn't a branch's target", r.symbol));
                continue;
            }
            sym_branch[k] = Some(r.symbol);
        }
    }
    for (k, i) in insns.iter().enumerate() {
        if let Some(f) = i.pc_rel_field
            && !relocs.iter().any(|r| r.offset == f)
        {
            errors.push(format!("PC-relative reference at {:#x} without a relocation", i.at));
        }
        if sym_branch[k].is_none()
            && let Some(t) = i.target
            && !(t >= 0 && starts.contains(&(t as usize)))
        {
            errors.push(format!("branch at {:#x} to {t:#x}, not an instruction of the stencil", i.at));
        }
    }
    if !errors.is_empty() {
        return errors;
    }

    // Stores reaching an exit.
    if STORES_BEFORE_EXIT.iter().any(|&(n, _)| n == name) {
        return errors;
    }
    let succ = |k: usize| -> Vec<usize> {
        let i = &insns[k];
        let mut s = Vec::new();
        if !i.ends_flow && k + 1 < insns.len() {
            s.push(k + 1);
        }
        if sym_branch[k].is_none()
            && let Some(t) = i.target
            && let Some(j) = index_of(t as usize)
        {
            s.push(j);
        }
        s
    };
    for k in (0..insns.len()).filter(|&k| insns[k].store) {
        let mut seen = vec![false; insns.len()];
        let mut stack = vec![k];
        while let Some(x) = stack.pop() {
            if std::mem::replace(&mut seen[x], true) {
                continue;
            }
            if sym_branch[x] == Some("cloison_exit") {
                errors.push(format!(
                    "the store at {:#x} reaches the exit at {:#x}: an exit must leave before any change \
                     (or the stencil be in `STORES_BEFORE_EXIT`, with why that is safe)",
                    insns[k].at, insns[x].at
                ));
                return errors;
            }
            stack.extend(succ(x));
        }
    }
    errors
}

/// What the checks need of an instruction.
pub struct Insn {
    pub at: usize,
    pub len: usize,
    /// A branch or call (its target a relocation, or `target`).
    pub branch: bool,
    /// Its local target, if a direct branch or call (with a relocation,
    /// its target is a symbol instead).
    pub target: Option<i64>,
    /// Nothing follows it: an unconditional branch, a return, a trap.
    pub ends_flow: bool,
    /// Where the field of a PC-relative data reference is.
    pub pc_rel_field: Option<usize>,
    /// It may write memory, but the native stack.
    pub store: bool,
}

pub mod x86 {
    use super::Insn;
    use iced_x86::{Decoder, DecoderOptions, FlowControl, InstructionInfoFactory, OpAccess, Register};

    /// Decode `code` to its end.
    pub fn decode(code: &[u8]) -> Result<Vec<Insn>, String> {
        let mut d = Decoder::with_ip(64, code, 0, DecoderOptions::NONE);
        let mut info = InstructionInfoFactory::new();
        let mut out = Vec::new();
        while d.can_decode() {
            let i = d.decode();
            let at = i.ip() as usize;
            if i.is_invalid() {
                return Err(format!("no instruction decodes at {at:#x}"));
            }
            let flow = i.flow_control();
            let direct = matches!(flow, FlowControl::UnconditionalBranch | FlowControl::ConditionalBranch);
            let pc_rel_field = i
                .is_ip_rel_memory_operand()
                .then(|| at + d.get_constant_offsets(&i).displacement_offset());
            let store = info.info(&i).used_memory().iter().any(|m| {
                matches!(m.access(), OpAccess::Write | OpAccess::CondWrite | OpAccess::ReadWrite | OpAccess::ReadCondWrite)
                    && m.base() != Register::RSP
            });
            out.push(Insn {
                at,
                len: i.len(),
                branch: direct || flow == FlowControl::Call,
                target: (direct || flow == FlowControl::Call).then(|| i.near_branch_target() as i64),
                ends_flow: matches!(
                    flow,
                    FlowControl::UnconditionalBranch | FlowControl::IndirectBranch | FlowControl::Return | FlowControl::Exception
                ),
                pc_rel_field,
                store,
            });
        }
        if d.position() != code.len() {
            return Err(format!("the last instruction ends past the code ({:#x})", code.len()));
        }
        Ok(out)
    }

    /// A `jmp rel32` or `jcc rel32` at `i` (its rel32 field the last four
    /// bytes): what a relocation can retarget.
    pub fn is_rel32_jump(code: &[u8], i: &Insn) -> bool {
        (i.len == 5 && code[i.at] == 0xE9) || (i.len == 6 && code[i.at] == 0x0F && (0x80..=0x8F).contains(&code[i.at + 1]))
    }
}

pub mod arm64 {
    use super::Insn;

    /// Decode `code` (fixed-width) into what the checks need.
    pub fn decode(code: &[u8]) -> Result<Vec<Insn>, String> {
        if !code.len().is_multiple_of(4) {
            return Err(format!("{} bytes: not whole instructions", code.len()));
        }
        Ok(code
            .chunks(4)
            .enumerate()
            .map(|(k, w)| {
                let i = u32::from_le_bytes(w.try_into().unwrap());
                let at = 4 * k;
                let disp = |bits: u32, shift: u32| {
                    let v = ((i >> shift) & ((1 << bits) - 1)) as i64;
                    (v << (64 - bits) >> (64 - bits)) * 4
                };
                let (target, branch) = if i & 0xFC00_0000 == 0x1400_0000 {
                    (Some(disp(26, 0)), true) // b
                } else if i & 0xFC00_0000 == 0x9400_0000 {
                    (None, true) // bl
                } else if i & 0xFF00_0010 == 0x5400_0000 || i & 0x7E00_0000 == 0x3400_0000 {
                    (Some(disp(19, 5)), true) // b.cond, cbz/cbnz
                } else if i & 0x7E00_0000 == 0x3600_0000 {
                    (Some(disp(14, 5)), true) // tbz/tbnz
                } else {
                    (None, false)
                };
                let ret_br = i & 0xFFFF_FC1F == 0xD65F_0000 || i & 0xFFFF_FC1F == 0xD61F_0000;
                let trap = i & 0xFFE0_001F == 0xD420_0000 || i >> 16 == 0;
                let pc_rel = i & 0x1F00_0000 == 0x1000_0000 // adr, adrp
                    || i & 0x3B00_0000 == 0x1800_0000; // ldr (literal)
                Insn {
                    at,
                    len: 4,
                    branch,
                    target: target.map(|d| at as i64 + d),
                    ends_flow: i & 0xFC00_0000 == 0x1400_0000 || ret_br || trap,
                    pc_rel_field: pc_rel.then_some(at),
                    store: is_store(i) && (i >> 5) & 31 != 31,
                }
            })
            .collect())
    }

    /// Whether `i` may write memory (its base register aside).
    fn is_store(i: u32) -> bool {
        // Loads and stores: op0 `x1x0` (bits 28 to 25).
        if i & 0x0A00_0000 != 0x0800_0000 {
            return false;
        }
        let l = i >> 22 & 1;
        match i >> 27 & 7 {
            // Pairs: `L` (bit 22) 0 stores.
            0b101 => l == 0,
            // Single registers: `opc` (bits 23-22) 00 stores, and a
            // vector `opc` 10 of size 00 (a `q` register). Atomics
            // (bit 21 with bits 11-10 00, not unsigned-offset) write too.
            0b111 => {
                let opc = i >> 22 & 3;
                let vector = i >> 26 & 1 == 1;
                let atomic = i >> 24 & 1 == 0 && i >> 21 & 1 == 1 && i >> 10 & 3 == 0;
                opc == 0 || vector && opc == 2 && i >> 30 == 0 || atomic
            }
            // Literal loads.
            0b011 => false,
            // Exclusive and ordered ones, and vector structures: `L`.
            _ => l == 0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Reloc, check};

    fn r(offset: usize, r_type: u32, symbol: &str) -> Reloc<'_> {
        Reloc { offset, r_type, symbol }
    }
    fn x86(code: &[u8], relocs: &[Reloc]) -> Vec<String> {
        check("t", "x86_64", code, relocs)
    }
    fn arm64(words: &[u32], relocs: &[Reloc]) -> Vec<String> {
        let code: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
        check("t", "aarch64", &code, relocs)
    }
    fn fails(errors: Vec<String>, what: &str) {
        assert!(errors.iter().any(|e| e.contains(what)), "expected `{what}`, got {errors:?}");
    }
    const PC32: u32 = 2;
    const JUMP26: u32 = 282;
    const CONDBR19: u32 = 280;

    // `jmp rel32` to the continuation.
    const JMP: [u8; 5] = [0xE9, 0, 0, 0, 0];
    // `jne rel32` (to an exit).
    const JNE: [u8; 6] = [0x0F, 0x85, 0, 0, 0, 0];

    #[test]
    fn x86_stencils_the_jit_can_copy_pass() {
        assert!(x86(&JMP, &[r(1, PC32, "cloison_continue")]).is_empty());
        // A store to the native stack before an exit is no change.
        let code = [&[0x48, 0x89, 0x04, 0x24][..], &JNE, &JMP].concat();
        assert!(x86(&code, &[r(6, PC32, "cloison_exit"), r(11, PC32, "cloison_continue")]).is_empty());
        // A store after it is fine.
        let code = [&JNE[..], &[0x48, 0x89, 0x07], &JMP].concat();
        assert!(x86(&code, &[r(2, PC32, "cloison_exit"), r(10, PC32, "cloison_continue")]).is_empty());
    }

    #[test]
    fn x86_violations_are_found() {
        fails(x86(&[0x90], &[]), "falls off");
        fails(x86(&[0x06], &[]), "no instruction decodes");
        // A truncated `jmp rel32`.
        fails(x86(&[0xE9, 0, 0], &[]), "no instruction decodes");
        // `mov qword [rdi], rax`, then an exit.
        let code = [&[0x48, 0x89, 0x07][..], &JNE, &JMP].concat();
        fails(x86(&code, &[r(5, PC32, "cloison_exit"), r(10, PC32, "cloison_continue")]), "reaches the exit");
        // `jmp +1`: into the middle of the next instruction.
        let code = [&[0xEB, 0x01][..], &JMP].concat();
        fails(x86(&code, &[r(3, PC32, "cloison_continue")]), "not an instruction of the stencil");
        // A continuation in `mov eax, imm32`.
        let code = [&[0xB8, 0, 0, 0, 0][..], &JMP].concat();
        fails(x86(&code, &[r(1, PC32, "cloison_continue"), r(6, PC32, "cloison_continue")]), "isn't a branch's target");
        // `mov rax, [rip]`: data the JIT doesn't copy.
        let code = [&[0x48, 0x8B, 0x05, 0, 0, 0, 0][..], &JMP].concat();
        fails(x86(&code, &[r(8, PC32, "cloison_continue")]), "without a relocation");
        // A relocation across two instructions.
        fails(x86(&[&[0x90][..], &JMP].concat(), &[r(0, PC32, "cloison_hole_imm")]), "within one instruction");
    }

    const B: u32 = 0x1400_0000;
    const B_NE: u32 = 0x5400_0001;

    #[test]
    fn arm64_stencils_the_jit_can_copy_pass() {
        assert!(arm64(&[B], &[r(0, JUMP26, "cloison_continue")]).is_empty());
        let exits = [r(4, CONDBR19, "cloison_exit"), r(8, JUMP26, "cloison_continue")];
        // `str x1, [sp]` and `ldr x1, [x0]` before an exit change nothing.
        assert!(arm64(&[0xF900_03E1, B_NE, B], &exits).is_empty());
        assert!(arm64(&[0xF940_0001, B_NE, B], &exits).is_empty());
    }

    #[test]
    fn arm64_violations_are_found() {
        fails(arm64(&[0xD503_201F], &[]), "falls off");
        let exits = [r(4, CONDBR19, "cloison_exit"), r(8, JUMP26, "cloison_continue")];
        // `str x1, [x0]`, `stp x0, x1, [x2]`, `str q0, [x0]`.
        for store in [0xF900_0001, 0xA900_0440, 0x3D80_0000] {
            fails(arm64(&[store, B_NE, B], &exits), "reaches the exit");
        }
        // `b.eq +16`, past the end.
        fails(arm64(&[0x5400_0080, B], &[r(4, JUMP26, "cloison_continue")]), "not an instruction of the stencil");
        // `ldr x0, <literal>`.
        fails(arm64(&[0x5800_0000, B], &[r(4, JUMP26, "cloison_continue")]), "without a relocation");
        // A continuation on an `add`.
        fails(arm64(&[0x9100_0000, B], &[r(0, JUMP26, "cloison_continue"), r(4, JUMP26, "cloison_continue")]), "isn't a branch's target");
    }
}
