//! Copy-and-patch baseline JIT (see `docs/future-work.md`, milestones 1–2).
//!
//! Each reachable bytecode instruction becomes a copy of a stencil
//! (`stencils/stencils.rs`) with its holes patched. Stencils are chosen per
//! instruction from inferred types (milestone 2):
//!
//! 1. [`infer`]: forward dataflow from the entry instruction over the
//!    lattice `I32 < F64 < Any`, for `acc` and each register, seeded with the
//!    exact types of the live values at entry.
//! 2. [`select`]: a specialized stencil without type checks when operand
//!    types are known (`add_ii`, `add_ff`, `jump_if_less_smi_i`, …), the
//!    generic one otherwise. int32 arithmetic side-exits on overflow (see
//!    [`JitFunction`]).
//! 3. Where a join widened a type from `I32` to `F64`, the edge into it gets
//!    a conversion stencil: inline for fall-through edges, in a cold stub for
//!    jumps.
//!
//! Layout of the generated code:
//!
//! ```text
//! [ hot: entry conversions; per instruction: fall-through conversions,  ]
//! [      then the instruction's stencil                                  ]
//! [ cold: edge stubs, exit stubs, `__slow` helpers                       ]
//! [ trampolines: one per runtime function (`toi_rt_*`) referenced       ]
//! ```
//!
//! A stencil whose last instruction jumps to `toi_continue` falls through
//! into the next piece with that jump trimmed. Calls to runtime functions
//! go through trampolines, so `rel32` / `CALL26` relocations only ever reach
//! inside JIT memory. On arm64, operand holes are `adrp`+`add` pairs whose
//! "address" is the operand value. `adrp` is page-relative with a ±4 GiB
//! range, so JIT memory is mapped below 4 GiB there.

use crate::bytecode::Op;
use crate::value::{NumberTag, Value};
use std::collections::{HashMap, HashSet};

/// A relocation in a stencil, from `build.rs`.
pub(crate) struct Reloc {
    /// Offset of the patched field within the stencil.
    offset: u32,
    /// ELF relocation type for the target architecture.
    r_type: u32,
    /// `toi_hole_*`, `toi_continue`, `toi_jump_target`, `toi_rt_*`, or
    /// `@<stencil>` for another stencil (a helper).
    symbol: &'static str,
    addend: i64,
}

pub(crate) struct Stencil {
    name: &'static str,
    code: &'static [u8],
    relocs: &'static [Reloc],
}

include!(concat!(env!("OUT_DIR"), "/stencils.rs"));

fn stencil(name: &str) -> &'static Stencil {
    STENCILS
        .iter()
        .find(|s| s.name == name)
        .unwrap_or_else(|| panic!("no stencil `{name}`"))
}

/// The `<name>__*` helpers of stencil `name`.
fn helpers(name: &str) -> impl Iterator<Item = &'static Stencil> {
    STENCILS.iter().filter(move |s| {
        s.name
            .strip_prefix(name)
            .is_some_and(|rest| rest.starts_with("__"))
    })
}

// ---- Runtime functions called from stencil slow paths (C ABI) ----

extern "C" fn toi_rt_add(a: Value, b: Value) -> Value {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => Value::from_f64_arith(x + y),
        _ => crate::interp::generic_add(a, b),
    }
}

extern "C" fn toi_rt_inc(a: Value) -> Value {
    match a.as_number() {
        Some(x) => Value::from_f64_arith(x + 1.0),
        None => crate::interp::generic_inc(a),
    }
}

extern "C" fn toi_rt_less_than(a: Value, b: Value) -> bool {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => x < y,
        _ => crate::interp::generic_less_than(a, b),
    }
}

fn runtime_function(name: &str) -> usize {
    match name {
        "toi_rt_add" => toi_rt_add as *const () as usize,
        "toi_rt_inc" => toi_rt_inc as *const () as usize,
        "toi_rt_less_than" => toi_rt_less_than as *const () as usize,
        _ => panic!("unknown runtime function `{name}`"),
    }
}

// ---- Architecture specifics ----

#[cfg(target_arch = "x86_64")]
mod arch {
    /// Length of the trailing `jmp rel32` dropped on fall-through.
    pub const TRAILING_JUMP: usize = 5;
    pub const TRAMPOLINE_SIZE: usize = 14;
    pub const CODE_ALIGN: usize = 16;

    /// Whether the stencil ends with `jmp toi_continue`.
    pub fn ends_with_continue(code: &[u8], relocs: &[super::Reloc]) -> bool {
        let n = code.len();
        n >= 5
            && code[n - 5] == 0xE9
            && relocs
                .iter()
                .any(|r| r.symbol == "toi_continue" && r.offset as usize == n - 4)
    }

    /// `jmp [rip+0]` followed by the 64-bit target.
    pub fn write_trampoline(buf: &mut [u8], target: u64) {
        buf[..6].copy_from_slice(&[0xFF, 0x25, 0, 0, 0, 0]);
        buf[6..14].copy_from_slice(&target.to_le_bytes());
    }

    /// Apply relocation `r_type` at `buf[0..]` (the field), located at
    /// address `p`, for symbol value `s` and addend `a`.
    pub fn patch(buf: &mut [u8], r_type: u32, s: u64, a: i64, p: u64) {
        let v = s.wrapping_add(a as u64);
        match r_type {
            // R_X86_64_64
            1 => buf[..8].copy_from_slice(&v.to_le_bytes()),
            // R_X86_64_PC32, R_X86_64_PLT32
            2 | 4 => {
                let d = v.wrapping_sub(p) as i64;
                let d = i32::try_from(d).expect("rel32 out of range");
                buf[..4].copy_from_slice(&d.to_le_bytes());
            }
            // R_X86_64_32, R_X86_64_32S: operand holes. Stencils only use the
            // low 32 bits, so the same bits work for both extensions.
            10 | 11 => {
                let v = u32::try_from(v).expect("32-bit hole out of range");
                buf[..4].copy_from_slice(&v.to_le_bytes());
            }
            _ => panic!("unsupported x86-64 relocation type {r_type}"),
        }
    }

    pub fn flush_icache(_mem: *const u8, _len: usize) {
        // x86 instruction caches are coherent with data writes.
    }
}

#[cfg(target_arch = "aarch64")]
mod arch {
    const R_AARCH64_ABS64: u32 = 257;
    const R_AARCH64_ADR_PREL_PG_HI21: u32 = 275;
    const R_AARCH64_ADD_ABS_LO12_NC: u32 = 277;
    const R_AARCH64_LDST8_ABS_LO12_NC: u32 = 278;
    const R_AARCH64_TSTBR14: u32 = 279;
    const R_AARCH64_CONDBR19: u32 = 280;
    const R_AARCH64_JUMP26: u32 = 282;
    const R_AARCH64_CALL26: u32 = 283;
    const R_AARCH64_LDST16_ABS_LO12_NC: u32 = 284;
    const R_AARCH64_LDST32_ABS_LO12_NC: u32 = 285;
    const R_AARCH64_LDST64_ABS_LO12_NC: u32 = 286;
    const R_AARCH64_LDST128_ABS_LO12_NC: u32 = 299;

    pub const TRAILING_JUMP: usize = 4;
    pub const TRAMPOLINE_SIZE: usize = 16;
    pub const CODE_ALIGN: usize = 16;

    pub fn ends_with_continue(code: &[u8], relocs: &[super::Reloc]) -> bool {
        let n = code.len();
        n >= 4
            && relocs.iter().any(|r| {
                r.symbol == "toi_continue"
                    && r.offset as usize == n - 4
                    && r.r_type == R_AARCH64_JUMP26
            })
    }

    /// `ldr x16, #8; br x16` followed by the 64-bit target.
    pub fn write_trampoline(buf: &mut [u8], target: u64) {
        buf[0..4].copy_from_slice(&0x5800_0050u32.to_le_bytes());
        buf[4..8].copy_from_slice(&0xD61F_0200u32.to_le_bytes());
        buf[8..16].copy_from_slice(&target.to_le_bytes());
    }

    fn read(buf: &[u8]) -> u32 {
        u32::from_le_bytes(buf[..4].try_into().unwrap())
    }

    fn write(buf: &mut [u8], insn: u32) {
        buf[..4].copy_from_slice(&insn.to_le_bytes());
    }

    /// A pc-relative branch displacement in instructions, checked to fit
    /// `bits` signed bits.
    fn branch_disp(v: u64, p: u64, bits: u32) -> u32 {
        let d = v.wrapping_sub(p) as i64;
        assert!(d & 3 == 0, "misaligned branch target");
        let d = d >> 2;
        let limit = 1i64 << (bits - 1);
        assert!((-limit..limit).contains(&d), "branch out of range");
        (d as u32) & ((1 << bits) - 1)
    }

    pub fn patch(buf: &mut [u8], r_type: u32, s: u64, a: i64, p: u64) {
        let v = s.wrapping_add(a as u64);
        match r_type {
            R_AARCH64_ABS64 => buf[..8].copy_from_slice(&v.to_le_bytes()),
            R_AARCH64_JUMP26 | R_AARCH64_CALL26 => {
                write(buf, (read(buf) & !0x03FF_FFFF) | branch_disp(v, p, 26));
            }
            R_AARCH64_CONDBR19 => {
                write(
                    buf,
                    (read(buf) & !(0x7FFFF << 5)) | (branch_disp(v, p, 19) << 5),
                );
            }
            R_AARCH64_TSTBR14 => {
                write(
                    buf,
                    (read(buf) & !(0x3FFF << 5)) | (branch_disp(v, p, 14) << 5),
                );
            }
            R_AARCH64_ADR_PREL_PG_HI21 => {
                let pages = ((v & !0xFFF) as i64 - (p & !0xFFF) as i64) >> 12;
                assert!(
                    (-(1 << 20)..(1 << 20)).contains(&pages),
                    "adrp out of range"
                );
                let (lo, hi) = ((pages as u32) & 3, ((pages as u32) >> 2) & 0x7FFFF);
                write(
                    buf,
                    (read(buf) & !(3 << 29 | 0x7FFFF << 5)) | lo << 29 | hi << 5,
                );
            }
            R_AARCH64_ADD_ABS_LO12_NC => {
                write(
                    buf,
                    (read(buf) & !(0xFFF << 10)) | ((v as u32 & 0xFFF) << 10),
                );
            }
            R_AARCH64_LDST8_ABS_LO12_NC
            | R_AARCH64_LDST16_ABS_LO12_NC
            | R_AARCH64_LDST32_ABS_LO12_NC
            | R_AARCH64_LDST64_ABS_LO12_NC
            | R_AARCH64_LDST128_ABS_LO12_NC => {
                let scale = match r_type {
                    R_AARCH64_LDST8_ABS_LO12_NC => 0,
                    R_AARCH64_LDST16_ABS_LO12_NC => 1,
                    R_AARCH64_LDST32_ABS_LO12_NC => 2,
                    R_AARCH64_LDST64_ABS_LO12_NC => 3,
                    _ => 4,
                };
                let lo12 = v as u32 & 0xFFF;
                assert!(lo12.is_multiple_of(1 << scale), "misaligned lo12 offset");
                write(buf, (read(buf) & !(0xFFF << 10)) | ((lo12 >> scale) << 10));
            }
            _ => panic!("unsupported AArch64 relocation type {r_type}"),
        }
    }

    pub fn flush_icache(mem: *const u8, len: usize) {
        // Clean the data cache to the point of unification, then invalidate
        // the instruction cache, line by line (CTR_EL0 gives line sizes).
        unsafe {
            let ctr: u64;
            core::arch::asm!("mrs {}, ctr_el0", out(reg) ctr);
            let dline = 4usize << ((ctr >> 16) & 0xF);
            let iline = 4usize << (ctr & 0xF);
            let (start, end) = (mem as usize, mem as usize + len);
            let mut a = start & !(dline - 1);
            while a < end {
                core::arch::asm!("dc cvau, {}", in(reg) a);
                a += dline;
            }
            core::arch::asm!("dsb ish");
            let mut a = start & !(iline - 1);
            while a < end {
                core::arch::asm!("ic ivau, {}", in(reg) a);
                a += iline;
            }
            core::arch::asm!("dsb ish", "isb");
        }
    }
}

// ---- Executable memory ----

/// An anonymous RW mapping, made RX by [`ExecMem::seal`].
struct ExecMem {
    ptr: *mut u8,
    len: usize,
}

impl ExecMem {
    fn new(len: usize) -> Self {
        let len = len.max(1).next_multiple_of(4096);
        let prot = libc::PROT_READ | libc::PROT_WRITE;
        let flags = libc::MAP_PRIVATE | libc::MAP_ANONYMOUS;
        // arm64 operand holes need JIT memory within adrp range (±4 GiB) of
        // small values, i.e. below 4 GiB. Walk hints through the low 4 GiB;
        // the kernel honours a hint when that range is free.
        let hints: Vec<usize> = if cfg!(target_arch = "aarch64") {
            (0x1000_0000..1 << 32).step_by(0x100_0000).collect()
        } else {
            vec![0]
        };
        for hint in hints {
            let ptr = unsafe { libc::mmap(hint as *mut _, len, prot, flags, -1, 0) };
            assert!(ptr != libc::MAP_FAILED, "mmap failed");
            if cfg!(target_arch = "x86_64") || (ptr as usize) + len <= 1 << 32 {
                return ExecMem {
                    ptr: ptr.cast(),
                    len,
                };
            }
            unsafe { libc::munmap(ptr, len) };
        }
        panic!("could not map JIT memory below 4 GiB");
    }

    fn bytes(&mut self) -> &mut [u8] {
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }

    fn seal(&mut self) {
        arch::flush_icache(self.ptr, self.len);
        let r =
            unsafe { libc::mprotect(self.ptr.cast(), self.len, libc::PROT_READ | libc::PROT_EXEC) };
        assert_eq!(r, 0, "mprotect failed");
    }
}

impl Drop for ExecMem {
    fn drop(&mut self) {
        unsafe { libc::munmap(self.ptr.cast(), self.len) };
    }
}

// ---- Decoding ----

#[derive(Clone, Copy)]
struct Insn {
    /// Bytecode offset.
    pc: usize,
    op: Op,
    /// `toi_hole_imm`: the immediate operand (1 for `inc`).
    imm: i32,
    /// `toi_hole_reg`: the register operand, if any.
    reg: u8,
    /// Instruction index of the jump target, if any.
    target: usize,
}

impl Insn {
    fn falls_through(&self) -> bool {
        !matches!(self.op, Op::Jump | Op::Return)
    }

    fn jumps(&self) -> bool {
        matches!(self.op, Op::Jump | Op::JumpIfLess | Op::JumpIfLessSmi)
    }

    fn reads_reg(&self) -> bool {
        matches!(self.op, Op::Ldar | Op::Star | Op::Add | Op::JumpIfLess)
    }
}

fn decode(code: &[u8]) -> Vec<Insn> {
    let imm_at = |at: usize| i32::from_le_bytes(code[at..at + 4].try_into().unwrap());
    let mut starts = HashMap::new();
    let mut raw = Vec::new();
    let mut pc = 0;
    while pc < code.len() {
        let op = Op::from_u8(code[pc]).unwrap_or_else(|| panic!("bad opcode {} at {pc}", code[pc]));
        starts.insert(pc, raw.len());
        raw.push((pc, op));
        pc += op.size();
    }
    assert_eq!(pc, code.len(), "truncated instruction");
    let index_of = |pc: usize, off: i32| {
        let to = pc
            .checked_add_signed(off as isize)
            .expect("jump out of range");
        *starts
            .get(&to)
            .unwrap_or_else(|| panic!("jump into the middle of an instruction at {to}"))
    };
    raw.into_iter()
        .map(|(pc, op)| {
            let (mut imm, mut reg, mut target) = (0, 0, 0);
            match op {
                Op::LdaSmi | Op::AddSmi => imm = imm_at(pc + 1),
                Op::Inc => imm = 1,
                Op::Ldar | Op::Star | Op::Add => reg = code[pc + 1],
                Op::JumpIfLess => (reg, target) = (code[pc + 1], index_of(pc, imm_at(pc + 2))),
                Op::JumpIfLessSmi => (imm, target) = (imm_at(pc + 1), index_of(pc, imm_at(pc + 5))),
                Op::Jump => target = index_of(pc, imm_at(pc + 1)),
                Op::Nop | Op::Return => {}
            }
            Insn {
                pc,
                op,
                imm,
                reg,
                target,
            }
        })
        .collect()
}

// ---- Type inference ----

/// The type lattice: `Bot < I32 < F64 < Any`. The join of `I32` and `F64`
/// is `F64`: an int32 converts losslessly to a double, so an edge carrying
/// `I32` into an `F64` point gets a conversion stencil.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Ty {
    Bot,
    I32,
    F64,
    Any,
}

impl Ty {
    /// The exact type of a runtime value.
    pub fn of(v: Value) -> Ty {
        if v.is_int32() {
            Ty::I32
        } else if v.is_double() {
            Ty::F64
        } else {
            Ty::Any
        }
    }
}

/// Types of the accumulator and of every register, at an instruction
/// boundary.
#[derive(Clone, PartialEq, Eq, Hash, Debug)]
pub struct TypeState {
    pub acc: Ty,
    pub regs: Vec<Ty>,
}

impl TypeState {
    fn of(acc: Value, regs: &[Value]) -> TypeState {
        TypeState {
            acc: Ty::of(acc),
            regs: regs.iter().map(|&v| Ty::of(v)).collect(),
        }
    }

    fn any(nregs: usize) -> TypeState {
        TypeState {
            acc: Ty::Any,
            regs: vec![Ty::Any; nregs],
        }
    }

    /// Join `other` into `self`; returns whether `self` changed.
    fn join(&mut self, other: &TypeState) -> bool {
        let mut changed = false;
        for (a, b) in std::iter::once((&mut self.acc, &other.acc))
            .chain(self.regs.iter_mut().zip(&other.regs))
        {
            if *b > *a {
                *a = *b;
                changed = true;
            }
        }
        changed
    }
}

/// A representation change on a control-flow edge.
#[derive(Clone, Copy)]
enum Conv {
    Acc,
    Reg(u8),
}

/// Conversions needed to go from `from` (an edge's source) to `to` (the
/// target's joined in-state). Types only widen along edges, and only
/// `I32 → F64` changes the representation.
fn conversions(from: &TypeState, to: &TypeState) -> Vec<Conv> {
    debug_assert!(from.acc <= to.acc && from.regs.iter().zip(&to.regs).all(|(a, b)| a <= b));
    let widens = |a: Ty, b: Ty| a == Ty::I32 && b == Ty::F64;
    let mut out = Vec::new();
    if widens(from.acc, to.acc) {
        out.push(Conv::Acc);
    }
    for (r, (&a, &b)) in from.regs.iter().zip(&to.regs).enumerate() {
        if widens(a, b) {
            out.push(Conv::Reg(r as u8));
        }
    }
    out
}

/// The stencil for `insn` given its in-state, and its out-state.
/// `widened` holds the instructions whose int32 result overflowed before;
/// they produce a double instead of exiting. `None` means no
/// specialization: everything is `Any`, so every stencil is generic.
fn select(
    insn: &Insn,
    i: usize,
    st: &TypeState,
    widened: Option<&HashSet<usize>>,
) -> (&'static str, TypeState) {
    use Ty::{Any, F64, I32};
    let Some(widened) = widened else {
        let any = TypeState::any(st.regs.len());
        let (name, _) = select(insn, i, &any, Some(&HashSet::new()));
        return (name, any);
    };
    let mut out = st.clone();
    let reg = || st.regs[insn.reg as usize];
    // An int32 op: exits on overflow unless it overflowed before.
    let int_op = |name: &'static str, widened_name: &'static str| {
        if widened.contains(&i) {
            (widened_name, F64)
        } else {
            (name, I32)
        }
    };
    let name = match insn.op {
        Op::Nop => "nop",
        Op::Jump => "jump",
        Op::Return => "return",
        Op::LdaSmi => {
            out.acc = I32;
            "lda_smi"
        }
        Op::Ldar => {
            out.acc = reg();
            "ldar"
        }
        Op::Star => {
            out.regs[insn.reg as usize] = st.acc;
            "star"
        }
        Op::Add => {
            let (name, ty) = match (st.acc, reg()) {
                (I32, I32) => int_op("add_ii", "add_ii_f"),
                (I32, F64) => ("add_if", F64),
                (F64, I32) => ("add_fi", F64),
                (F64, F64) => ("add_ff", F64),
                _ => ("add", Any),
            };
            out.acc = ty;
            name
        }
        Op::AddSmi => {
            let (name, ty) = match st.acc {
                I32 => int_op("add_smi_i", "add_smi_i_f"),
                F64 => ("add_smi_f", F64),
                _ => ("add_smi", Any),
            };
            out.acc = ty;
            name
        }
        Op::Inc => {
            let (name, ty) = match st.acc {
                I32 => int_op("inc_i", "inc_i_f"),
                F64 => ("inc_f", F64),
                _ => ("inc", Any),
            };
            out.acc = ty;
            name
        }
        Op::JumpIfLess => match (st.acc, reg()) {
            (I32, I32) => "jump_if_less_ii",
            (I32, F64) => "jump_if_less_if",
            (F64, I32) => "jump_if_less_fi",
            (F64, F64) => "jump_if_less_ff",
            _ => "jump_if_less",
        },
        Op::JumpIfLessSmi => match st.acc {
            I32 => "jump_if_less_smi_i",
            F64 => "jump_if_less_smi_f",
            _ => "jump_if_less_smi",
        },
    };
    (name, out)
}

/// Forward dataflow from `entry` (instruction index) with `entry_state`.
/// Returns the joined in-state of every reachable instruction.
fn infer(
    insns: &[Insn],
    entry: usize,
    entry_state: TypeState,
    widened: Option<&HashSet<usize>>,
) -> Vec<Option<TypeState>> {
    let mut ins: Vec<Option<TypeState>> = vec![None; insns.len()];
    ins[entry] = Some(entry_state);
    let mut work = vec![entry];
    while let Some(i) = work.pop() {
        let (_, out) = select(&insns[i], i, ins[i].as_ref().unwrap(), widened);
        let succs = [
            insns[i].falls_through().then_some(i + 1),
            insns[i].jumps().then_some(insns[i].target),
        ];
        for s in succs.into_iter().flatten() {
            assert!(s < insns.len(), "control falls off the end of the bytecode");
            let changed = match &mut ins[s] {
                Some(st) => st.join(&out),
                slot @ None => {
                    *slot = Some(out.clone());
                    true
                }
            };
            if changed {
                work.push(s);
            }
        }
    }
    ins
}

// ---- Layout and patching ----

/// Where a stencil's `toi_continue` / `toi_jump_target` goes.
#[derive(Clone, Copy)]
enum Target {
    /// Whatever is laid out right after this piece (lets the jump be trimmed).
    Next,
    /// The main stencil of instruction `i`.
    Label(usize),
    /// Edge stub `n` (conversions, then a jump to its target).
    Stub(usize),
}

/// One stencil instance to emit.
struct Piece {
    stencil: &'static Stencil,
    imm: i32,
    reg: u8,
    cont: Target,
    jump: Target,
    /// Instruction whose exit stub `toi_exit` goes to.
    exit: usize,
}

impl Piece {
    fn conv(c: Conv, cont: Target) -> Piece {
        let (name, reg) = match c {
            Conv::Acc => ("conv_acc_f", 0),
            Conv::Reg(r) => ("conv_reg_f", r),
        };
        Piece {
            stencil: stencil(name),
            imm: 0,
            reg,
            cont,
            jump: Target::Next,
            exit: usize::MAX,
        }
    }

    fn convs(cs: &[Conv], last: Target) -> Vec<Piece> {
        cs.iter()
            .enumerate()
            .map(|(k, &c)| {
                Piece::conv(
                    c,
                    if k + 1 == cs.len() {
                        last
                    } else {
                        Target::Next
                    },
                )
            })
            .collect()
    }

    fn trims(&self) -> bool {
        matches!(self.cont, Target::Next)
            && arch::ends_with_continue(self.stencil.code, self.stencil.relocs)
    }

    fn len(&self) -> usize {
        self.stencil.code.len() - if self.trims() { arch::TRAILING_JUMP } else { 0 }
    }

    fn uses(&self, symbol: &str) -> bool {
        self.stencil.relocs.iter().any(|r| r.symbol == symbol)
    }
}

/// Value of `ctx` while running: no exit taken.
const NO_EXIT: u32 = u32::MAX;

type Entry = extern "rust-preserve-none" fn(Value, *mut Value, NumberTag, *mut u32) -> Value;

/// Machine code for one entry into a bytecode function.
pub struct JitCode {
    mem: ExecMem,
    entry: usize,
    /// Bytes of hot code, from the start (excluding cold code).
    main_len: usize,
    /// The selected stencil per instruction (`None`: unreachable from the
    /// entry), for inspection and tests.
    pub stencils: Vec<Option<&'static str>>,
}

/// How a run of [`JitCode`] ended.
pub enum Outcome {
    Return(Value),
    /// Side exit before the instruction at bytecode offset `pc`, with `acc`;
    /// the register file is up to date.
    Exit {
        pc: usize,
        acc: Value,
    },
}

impl JitCode {
    /// Compile `insns` for entry at instruction `entry` with types
    /// `entry_state`. With `specialize` off, every stencil is generic
    /// (milestone 1).
    fn compile(
        insns: &[Insn],
        entry: usize,
        entry_state: TypeState,
        widened: &HashSet<usize>,
        specialize: bool,
    ) -> JitCode {
        let n = insns.len();
        let entry_state = if specialize {
            entry_state
        } else {
            TypeState::any(entry_state.regs.len())
        };
        let widened = specialize.then_some(widened);
        let ins = infer(insns, entry, entry_state.clone(), widened);
        let sel: Vec<Option<(&'static str, TypeState)>> = (0..n)
            .map(|i| ins[i].as_ref().map(|st| select(&insns[i], i, st, widened)))
            .collect();

        // Hot code: entry conversions, then each reachable instruction in
        // bytecode order, preceded by the conversions on its fall-through
        // edge. Jump edges needing conversions get cold stubs.
        let mut hot: Vec<Piece> = Vec::new();
        let mut stubs: Vec<Vec<Piece>> = Vec::new();
        let mut label_piece = vec![usize::MAX; n];

        let entry_convs = conversions(&entry_state, ins[entry].as_ref().unwrap());
        hot.extend(Piece::convs(&entry_convs, Target::Label(entry)));

        for i in 0..n {
            let Some((name, out)) = &sel[i] else { continue };
            let insn = &insns[i];
            if i > 0
                && let Some((_, prev_out)) = &sel[i - 1]
                && insns[i - 1].falls_through()
            {
                hot.extend(Piece::convs(
                    &conversions(prev_out, ins[i].as_ref().unwrap()),
                    Target::Next,
                ));
            }
            let jump = if insn.jumps() {
                let cs = conversions(out, ins[insn.target].as_ref().unwrap());
                if cs.is_empty() {
                    Target::Label(insn.target)
                } else {
                    stubs.push(Piece::convs(&cs, Target::Label(insn.target)));
                    Target::Stub(stubs.len() - 1)
                }
            } else {
                Target::Next
            };
            label_piece[i] = hot.len();
            hot.push(Piece {
                stencil: stencil(name),
                imm: insn.imm,
                reg: insn.reg,
                cont: Target::Next,
                jump,
                exit: i,
            });
        }

        // Positions. Hot pieces back to back; then cold code: edge stubs,
        // exit stubs, helpers; then trampolines.
        let mut pos = 0;
        let hot_at: Vec<usize> = hot
            .iter()
            .map(|p| {
                let at = pos;
                pos += p.len();
                at
            })
            .collect();
        let main_len = pos;
        let label = |i: usize| hot_at[label_piece[i]];
        let entry_at = if entry_convs.is_empty() {
            label(entry)
        } else {
            0
        };

        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let mut stub_at = Vec::new();
        let mut stub_piece_at = Vec::new();
        for stub in &stubs {
            stub_at.push(pos);
            stub_piece_at.push(
                stub.iter()
                    .map(|p| {
                        let at = pos;
                        pos += p.len();
                        at
                    })
                    .collect::<Vec<_>>(),
            );
        }
        let mut exit_at: HashMap<usize, usize> = HashMap::new();
        for p in &hot {
            if p.uses("toi_exit") && !exit_at.contains_key(&p.exit) {
                exit_at.insert(p.exit, pos);
                pos += stencil("exit").code.len();
            }
        }
        // Helpers, per piece instance: (piece key, helper name) -> offset.
        let mut helper_at: HashMap<(usize, &str), usize> = HashMap::new();
        let mut helper_list = Vec::new();
        for (k, p) in hot.iter().enumerate() {
            for h in helpers(p.stencil.name) {
                helper_at.insert((k, h.name), pos);
                helper_list.push((k, h, pos));
                pos += h.code.len();
            }
        }
        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let mut tramp_at: HashMap<&str, usize> = HashMap::new();
        let referenced = hot
            .iter()
            .map(|p| p.stencil)
            .chain(helper_list.iter().map(|&(_, h, _)| h));
        for s in referenced {
            for r in s.relocs.iter().filter(|r| r.symbol.starts_with("toi_rt_")) {
                tramp_at.entry(r.symbol).or_insert_with(|| {
                    let at = pos;
                    pos += arch::TRAMPOLINE_SIZE;
                    at
                });
            }
        }

        let mut mem = ExecMem::new(pos);
        let base = mem.ptr as u64;
        let buf = mem.bytes();
        for (&name, &at) in &tramp_at {
            arch::write_trampoline(&mut buf[at..], runtime_function(name) as u64);
        }

        let resolve = |t: Target, next: usize| match t {
            Target::Next => next,
            Target::Label(i) => label(i),
            Target::Stub(s) => stub_at[s],
        };
        // Copy the first `len` bytes of `s` to `at` and patch it: operands
        // from `p`, continuation and branch target at the given offsets, and
        // helpers of hot piece `key`.
        let mut emit = |s: &Stencil,
                        len: usize,
                        at: usize,
                        p: &Piece,
                        cont: usize,
                        jump: usize,
                        key: Option<usize>| {
            buf[at..at + len].copy_from_slice(&s.code[..len]);
            for r in s.relocs.iter().filter(|r| (r.offset as usize) < len) {
                let value = match r.symbol {
                    "toi_hole_imm" => p.imm as u32 as u64,
                    "toi_hole_reg" => p.reg as u64 * size_of::<Value>() as u64,
                    "toi_continue" => base + cont as u64,
                    "toi_jump_target" => base + jump as u64,
                    "toi_exit" => base + exit_at[&p.exit] as u64,
                    sym if sym.starts_with("toi_rt_") => base + tramp_at[sym] as u64,
                    sym if sym.starts_with('@') => {
                        let k = key.expect("helper reference outside a hot piece");
                        let off = helper_at.get(&(k, &sym[1..])).unwrap_or_else(|| {
                            panic!(
                                "stencil {} references {sym}, not one of its helpers",
                                s.name
                            )
                        });
                        base + *off as u64
                    }
                    sym => panic!("unknown stencil symbol `{sym}`"),
                };
                let field = at + r.offset as usize;
                arch::patch(
                    &mut buf[field..],
                    r.r_type,
                    value,
                    r.addend,
                    base + field as u64,
                );
            }
        };
        // Resolved continuation and branch target of each hot piece; its
        // helpers share them.
        let hot_targets: Vec<(usize, usize)> = hot
            .iter()
            .zip(&hot_at)
            .map(|(p, &at)| (resolve(p.cont, at + p.len()), resolve(p.jump, at + p.len())))
            .collect();
        for (k, p) in hot.iter().enumerate() {
            let (cont, jump) = hot_targets[k];
            emit(p.stencil, p.len(), hot_at[k], p, cont, jump, Some(k));
        }
        for &(k, h, at) in &helper_list {
            let (cont, jump) = hot_targets[k];
            emit(h, h.code.len(), at, &hot[k], cont, jump, Some(k));
        }
        for (stub, ats) in stubs.iter().zip(&stub_piece_at) {
            for (p, &at) in stub.iter().zip(ats) {
                emit(
                    p.stencil,
                    p.len(),
                    at,
                    p,
                    resolve(p.cont, at + p.len()),
                    0,
                    None,
                );
            }
        }
        let exit_stencil = stencil("exit");
        for (&i, &at) in &exit_at {
            let p = Piece {
                stencil: exit_stencil,
                imm: insns[i].pc as i32,
                reg: 0,
                cont: Target::Next,
                jump: Target::Next,
                exit: i,
            };
            emit(exit_stencil, exit_stencil.code.len(), at, &p, 0, 0, None);
        }

        mem.seal();
        JitCode {
            mem,
            entry: entry_at,
            main_len,
            stencils: sel
                .iter()
                .map(|s| s.as_ref().map(|(name, _)| *name))
                .collect(),
        }
    }

    /// Run from the entry point over `regs`, starting with `acc`.
    ///
    /// # Safety
    /// `regs` must cover every register operand, and the code must have been
    /// compiled for the current types of `acc` and `regs`.
    unsafe fn enter(&self, acc: Value, regs: &mut [Value]) -> Outcome {
        let entry: Entry = unsafe { std::mem::transmute(self.mem.ptr.add(self.entry)) };
        let mut exit = NO_EXIT;
        let acc = entry(acc, regs.as_mut_ptr(), NumberTag::new(), &mut exit);
        match exit {
            NO_EXIT => Outcome::Return(acc),
            pc => Outcome::Exit {
                pc: pc as usize,
                acc,
            },
        }
    }

    /// The hot machine code, for inspection.
    pub fn main_code(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.mem.ptr, self.main_len) }
    }
}

// ---- Driver ----

/// Counters, for tests and benchmarks.
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct JitStats {
    pub compiles: u32,
    pub exits: u32,
}

/// A bytecode function run through the JIT.
///
/// Code is compiled when execution enters, for the exact types of the live
/// values at that point. So entry types are facts, not speculation, and no
/// entry guards are needed: inside the region, types only widen, and edges
/// get conversions. The only speculation is that int32 arithmetic doesn't
/// overflow. When it does, the stencil side-exits before the instruction.
/// The driver then records the instruction as widened (it will produce a
/// double), recompiles for entry at that instruction with the current
/// types, and resumes there. Code for entry at the start is cached, keyed by
/// entry types and invalidated when feedback changes.
pub struct JitFunction {
    code: Vec<u8>,
    insns: Vec<Insn>,
    nregs: usize,
    specialize: bool,
    widened: HashSet<usize>,
    /// Code for entry at instruction 0, and the entry types it was built for.
    cached: Option<(TypeState, JitCode)>,
    /// The most recently compiled code, for inspection.
    last: Option<JitCode>,
    pub stats: JitStats,
}

/// After this many compilations in one run, finish in the interpreter.
const MAX_COMPILES: u32 = 16;

impl JitFunction {
    /// Prepare `code`; nothing is compiled yet. Panics on malformed bytecode.
    pub fn new(code: &[u8]) -> JitFunction {
        let insns = decode(code);
        assert!(!insns.is_empty(), "empty bytecode");
        let nregs = insns
            .iter()
            .filter(|i| i.reads_reg())
            .map(|i| i.reg as usize + 1)
            .max()
            .unwrap_or(0);
        JitFunction {
            code: code.to_vec(),
            insns,
            nregs,
            specialize: true,
            widened: HashSet::new(),
            cached: None,
            last: None,
            stats: JitStats::default(),
        }
    }

    /// Use only generic stencils, as milestone 1 did.
    pub fn unspecialized(mut self) -> JitFunction {
        self.specialize = false;
        self
    }

    fn index_of(&self, pc: usize) -> usize {
        self.insns
            .iter()
            .position(|i| i.pc == pc)
            .expect("exit at an instruction boundary")
    }

    fn compile(&mut self, entry: usize, state: TypeState) -> JitCode {
        self.stats.compiles += 1;
        JitCode::compile(&self.insns, entry, state, &self.widened, self.specialize)
    }

    /// Run over `regs` and return the accumulator at `Return`. The
    /// accumulator starts as `undefined`.
    ///
    /// # Safety
    /// As [`crate::interp::run`]: register operands must be `< regs.len()`.
    pub unsafe fn run(&mut self, regs: &mut [Value]) -> Value {
        assert!(regs.len() >= self.nregs, "register file too small");
        let (mut pc, mut acc) = (0, Value::UNDEFINED);
        loop {
            let state = TypeState::of(acc, &regs[..self.nregs]);
            let entry = self.index_of(pc);
            if self.stats.compiles >= MAX_COMPILES {
                return unsafe { crate::interp::run_from(&self.code, pc, acc, regs) };
            }
            let outcome = if entry == 0 {
                if self.cached.as_ref().is_none_or(|(s, _)| *s != state) {
                    let code = self.compile(0, state.clone());
                    self.cached = Some((state, code));
                }
                unsafe { self.cached.as_ref().unwrap().1.enter(acc, regs) }
            } else {
                let code = self.compile(entry, state);
                let outcome = unsafe { code.enter(acc, regs) };
                self.last = Some(code);
                outcome
            };
            match outcome {
                Outcome::Return(v) => return v,
                Outcome::Exit {
                    pc: exit_pc,
                    acc: exit_acc,
                } => {
                    self.stats.exits += 1;
                    // Only int32 arithmetic exits: widen it and recompile.
                    self.widened.insert(self.index_of(exit_pc));
                    self.cached = None;
                    (pc, acc) = (exit_pc, exit_acc);
                }
            }
        }
    }

    /// The most recently compiled code.
    pub fn last_code(&self) -> Option<&JitCode> {
        self.last.as_ref().or(self.cached.as_ref().map(|(_, c)| c))
    }
}

/// Compile and run `code` once. See [`JitFunction::run`].
///
/// # Safety
/// As [`crate::interp::run`].
pub unsafe fn run(code: &[u8], regs: &mut [Value]) -> Value {
    unsafe { JitFunction::new(code).run(regs) }
}
