//! Copy-and-patch baseline JIT (see `docs/future-work.md`, milestones 1–3).
//!
//! Each reachable bytecode instruction becomes a copy of a stencil
//! (`stencils/stencils.rs`) with its holes patched. Stencils are chosen per
//! instruction from inferred types and register homes:
//!
//! 1. [`infer`]: forward dataflow from the entry instruction over the
//!    lattice `I32 < F64 < Any`, for `acc` and each register, seeded with the
//!    exact types of the live values at entry.
//! 2. [`assign_homes`] (milestone 3): each VM register lives, for the whole
//!    region, in an FP slot as an unboxed double (if it's only ever int32 or
//!    double, and at least once double; it's then *pinned* as double and
//!    inference runs again), in a GPR slot (boxed), or in memory. Slots go
//!    to the most used registers: [`GPR_SLOTS`] is 6 on x86-64 and 12 on
//!    arm64, [`FP_SLOTS`] 7 on both.
//! 3. [`select`] and [`stencil_for`]: a specialized stencil without type
//!    checks when operand types are known (`add_ii_g0`, `add_ff_f1`,
//!    `jlt_smi_i`, …), the generic one otherwise, for the operand's home.
//!    int32 arithmetic side-exits on overflow (see [`JitFunction`]). A double
//!    accumulator lives unboxed in `facc`.
//! 4. Where a join widens a type, the edge gets conversions: int32 → double,
//!    or `facc` → boxed `acc`. They go inline on fall-through edges and in a
//!    cold stub on jumps.
//!
//! Layout of the generated code:
//!
//! ```text
//! [ hot: entry (load cached registers, convert); per instruction:        ]
//! [      fall-through conversions, then (at its label) e.g. spills       ]
//! [      before `return`, then the instruction's stencil                 ]
//! [ cold: edge stubs, exit stubs (spill, then exit), `__slow` helpers    ]
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
    static BY_NAME: std::sync::OnceLock<HashMap<&'static str, &'static Stencil>> =
        std::sync::OnceLock::new();
    BY_NAME
        .get_or_init(|| STENCILS.iter().map(|s| (s.name, s)).collect())
        .get(name)
        .copied()
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
        let pairs = std::iter::once((&mut self.acc, &other.acc))
            .chain(self.regs.iter_mut().zip(&other.regs));
        for (a, b) in pairs {
            if *b > *a {
                *a = *b;
                changed = true;
            }
        }
        changed
    }
}

/// Inputs to stencil selection that are fixed for a compilation.
struct Spec<'a> {
    /// `None`: no specialization; every type is `Any`, every stencil generic.
    widened: Option<&'a HashSet<usize>>,
    /// Registers pinned as doubles (they live in FP slots).
    pinned: &'a [bool],
}

/// What [`select`] decided for one instruction: the stencil family, the
/// operand-type case (`"ii"`, `"f"`, … or `""` for generic) and the
/// out-state.
struct Sel {
    base: &'static str,
    case: &'static str,
    out: TypeState,
}

/// Choose the operation's type case from its in-state. Instructions in
/// `widened` overflowed int32 before, so they produce a double instead of
/// exiting. A store to a pinned register makes it `F64` (an int32 is
/// converted).
fn select(insn: &Insn, i: usize, st: &TypeState, spec: &Spec) -> Sel {
    use Ty::{Any, F64, I32};
    let Some(widened) = spec.widened else {
        let any = TypeState::any(st.regs.len());
        let no = Spec {
            widened: Some(&HashSet::new()),
            pinned: spec.pinned,
        };
        let Sel { base, case, .. } = select(insn, i, &any, &no);
        return Sel {
            base,
            case,
            out: any,
        };
    };
    let mut out = st.clone();
    let reg = st.regs.get(insn.reg as usize).copied().unwrap_or(Any);
    // An int32 op: exits on overflow unless it overflowed before.
    let int_op = |exits: &'static str, widened_case: &'static str| {
        if widened.contains(&i) {
            (widened_case, F64)
        } else {
            (exits, I32)
        }
    };
    let (base, case) = match insn.op {
        Op::Nop => ("nop", ""),
        Op::Jump => ("jump", ""),
        Op::Return => ("return", ""),
        Op::LdaSmi => {
            out.acc = I32;
            ("lda_smi", "")
        }
        Op::Ldar => {
            out.acc = reg;
            ("ldar", if reg == F64 { "f" } else { "" })
        }
        Op::Star => {
            let r = insn.reg as usize;
            out.regs[r] = if spec.pinned[r] && st.acc <= F64 {
                F64
            } else {
                st.acc
            };
            let case = match st.acc {
                F64 => "f",
                I32 => "i",
                _ => "",
            };
            ("star", case)
        }
        Op::Add => {
            let (case, ty) = match (st.acc, reg) {
                (I32, I32) => int_op("ii", "iif"),
                (I32, F64) => ("if", F64),
                (F64, I32) => ("fi", F64),
                (F64, F64) => ("ff", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            ("add", case)
        }
        Op::AddSmi | Op::Inc => {
            let base = if insn.op == Op::Inc { "inc" } else { "add_smi" };
            let (case, ty) = match st.acc {
                I32 => int_op("i", "iif"),
                F64 => ("f", F64),
                _ => ("", Any),
            };
            out.acc = ty;
            (base, case)
        }
        Op::JumpIfLess => {
            let case = match (st.acc, reg) {
                (I32, I32) => "ii",
                (I32, F64) => "if",
                (F64, I32) => "fi",
                (F64, F64) => "ff",
                _ => "",
            };
            ("jlt", case)
        }
        Op::JumpIfLessSmi => {
            let case = match st.acc {
                I32 => "i",
                F64 => "f",
                _ => "",
            };
            ("jlt_smi", case)
        }
    };
    Sel { base, case, out }
}

/// Forward dataflow from `entry` (instruction index) with `entry_state`.
/// Returns the joined in-state of every reachable instruction.
fn infer(
    insns: &[Insn],
    entry: usize,
    entry_state: TypeState,
    spec: &Spec,
) -> Vec<Option<TypeState>> {
    let mut ins: Vec<Option<TypeState>> = vec![None; insns.len()];
    ins[entry] = Some(entry_state);
    let mut work = vec![entry];
    while let Some(i) = work.pop() {
        let out = select(&insns[i], i, ins[i].as_ref().unwrap(), spec).out;
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

// ---- Register homes (milestone 3) ----

/// GPR slots `g0…` (boxed values) and FP slots `f0…` (unboxed doubles) in
/// the stencil signature; see `stencils/stencils.rs`.
#[cfg(target_arch = "x86_64")]
pub const GPR_SLOTS: usize = 6;
#[cfg(target_arch = "aarch64")]
pub const GPR_SLOTS: usize = 12;
pub const FP_SLOTS: usize = 7;

/// Where a VM register lives during a compiled region.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Home {
    /// The register file in memory, boxed.
    Mem,
    /// GPR slot `k`, boxed.
    Gpr(u8),
    /// FP slot `k`, an unboxed double. The register is pinned as `F64`.
    Fpr(u8),
}

impl Home {
    fn loc(self) -> String {
        match self {
            Home::Mem => "m".into(),
            Home::Gpr(k) => format!("g{k}"),
            Home::Fpr(k) => format!("f{k}"),
        }
    }
}

/// Assign homes by static use count. Registers whose type is only ever
/// int32 or double (and at least once double) get FP slots, unless in
/// `no_fp`; the rest get GPR slots; the rest stay in memory.
fn assign_homes(
    insns: &[Insn],
    ins: &[Option<TypeState>],
    nregs: usize,
    no_fp: &[bool],
) -> Vec<Home> {
    let mut uses = vec![0usize; nregs];
    let mut join = vec![Ty::Bot; nregs];
    for (insn, st) in insns.iter().zip(ins) {
        let Some(st) = st else { continue };
        if insn.reads_reg() {
            uses[insn.reg as usize] += 1;
        }
        for (j, &t) in join.iter_mut().zip(&st.regs) {
            *j = (*j).max(t);
        }
    }
    let mut by_uses: Vec<usize> = (0..nregs).filter(|&r| uses[r] > 0).collect();
    by_uses.sort_by_key(|&r| (std::cmp::Reverse(uses[r]), r));
    let mut homes = vec![Home::Mem; nregs];
    let (mut fp, mut gpr) = (0, 0);
    for &r in &by_uses {
        if join[r] == Ty::F64 && !no_fp[r] && fp < FP_SLOTS {
            homes[r] = Home::Fpr(fp as u8);
            fp += 1;
        }
    }
    for &r in &by_uses {
        if homes[r] == Home::Mem && gpr < GPR_SLOTS {
            homes[r] = Home::Gpr(gpr as u8);
            gpr += 1;
        }
    }
    homes
}

// ---- Layout and patching ----

/// Where a stencil's `toi_continue` / `toi_jump_target` goes.
#[derive(Clone, Copy)]
enum Target {
    /// Whatever is laid out right after this piece (lets the jump be trimmed).
    Next,
    /// The first piece of instruction `i`.
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
    fn new(name: &str, reg: u8) -> Piece {
        Piece {
            stencil: stencil(name),
            imm: 0,
            reg,
            cont: Target::Next,
            jump: Target::Next,
            exit: usize::MAX,
        }
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

/// Make the last piece of a sequence continue at `t` instead of falling
/// through.
fn ending_at(mut pieces: Vec<Piece>, t: Target) -> Vec<Piece> {
    if let Some(last) = pieces.last_mut() {
        last.cont = t;
    }
    pieces
}

/// Representation changes on an edge from `from` to `to` (types only
/// widen). The accumulator moves between `acc` (boxed) and `facc`
/// (unboxed double); a register in memory or a GPR slot converts its boxed
/// int32 to a boxed double. Pinned registers are always doubles.
fn edge_conversions(from: &TypeState, to: &TypeState, homes: &[Home]) -> Vec<Piece> {
    let mut out = Vec::new();
    match (from.acc, to.acc) {
        (Ty::I32, Ty::F64) => out.push(Piece::new("conv_acc_f", 0)),
        (Ty::F64, Ty::Any) => out.push(Piece::new("box_facc", 0)),
        _ => {}
    }
    for (r, (&a, &b)) in from.regs.iter().zip(&to.regs).enumerate() {
        if a == Ty::I32 && b == Ty::F64 && !matches!(homes[r], Home::Fpr(_)) {
            out.push(Piece::new(&format!("conv_{}_f", homes[r].loc()), r as u8));
        }
    }
    out
}

/// Entry: load cached registers from the register file (unboxing pinned
/// ones), then convert to the entry instruction's in-state. `actual` is the
/// exact entry state; `acc` arrives boxed.
fn entry_pieces(actual: &TypeState, to: &TypeState, homes: &[Home]) -> Vec<Piece> {
    let mut out = Vec::new();
    for (r, &h) in homes.iter().enumerate() {
        match h {
            Home::Mem => {}
            Home::Gpr(k) => out.push(Piece::new(&format!("enter_g{k}"), r as u8)),
            Home::Fpr(k) => {
                let kind = match actual.regs[r] {
                    Ty::I32 => "i",
                    Ty::F64 => "f",
                    t => panic!("register r{r} pinned as double but enters as {t:?}"),
                };
                out.push(Piece::new(&format!("enter_f{k}_{kind}"), r as u8));
            }
        }
    }
    match (actual.acc, to.acc) {
        (Ty::F64, Ty::F64) => out.push(Piece::new("unbox_facc", 0)),
        (Ty::I32, Ty::F64) => out.push(Piece::new("conv_acc_f", 0)),
        _ => {}
    }
    let mut regs_only = actual.clone();
    regs_only.acc = to.acc;
    out.extend(edge_conversions(&regs_only, to, homes));
    out
}

/// Write every cached register back to the register file, boxed.
fn spills(homes: &[Home]) -> Vec<Piece> {
    homes
        .iter()
        .enumerate()
        .filter_map(|(r, &h)| match h {
            Home::Mem => None,
            Home::Gpr(k) => Some(Piece::new(&format!("spill_g{k}"), r as u8)),
            Home::Fpr(k) => Some(Piece::new(&format!("spill_f{k}"), r as u8)),
        })
        .collect()
}

/// The stencil name for instruction `insn`, and pieces to run before it.
fn stencil_for(insn: &Insn, sel: &Sel, st: &TypeState, homes: &[Home]) -> (Vec<Piece>, String) {
    let mut pre = Vec::new();
    let home = homes.get(insn.reg as usize).copied().unwrap_or(Home::Mem);
    let loc = home.loc();
    // Generic stencils take a boxed acc.
    let box_acc = |pre: &mut Vec<Piece>| {
        if st.acc == Ty::F64 {
            pre.push(Piece::new("box_facc", 0));
        }
    };
    let name = match (sel.base, home) {
        ("ldar", Home::Fpr(k)) => format!("ldar_f{k}"),
        ("ldar", _) if sel.case == "f" => format!("ldar_{loc}_f"),
        ("ldar", _) => format!("ldar_{loc}"),
        ("star", Home::Fpr(k)) if sel.case == "f" => format!("star_f{k}"),
        ("star", Home::Fpr(k)) => format!("star_i_f{k}"),
        ("star", _) if sel.case == "f" => format!("star_{loc}_f"),
        ("star", _) => format!("star_{loc}"),
        ("add" | "jlt", _) if sel.case.is_empty() => {
            box_acc(&mut pre);
            format!("{}_{loc}", sel.base)
        }
        ("add" | "jlt", _) => format!("{}_{}_{loc}", sel.base, sel.case),
        ("return", _) => {
            box_acc(&mut pre);
            pre.extend(spills(homes));
            "return".into()
        }
        (base, _) if sel.case.is_empty() => base.into(),
        (base, _) => format!("{base}_{}", sel.case),
    };
    (pre, name)
}

/// Value of `ctx` while running: no exit taken.
const NO_EXIT: u32 = u32::MAX;

/// Call the generated code at `f` with the machine state: slots start empty
/// (`enter_*` stencils load them).
#[cfg(target_arch = "x86_64")]
unsafe fn call_entry(f: *const u8, acc: Value, regs: *mut Value, ctx: *mut u32) -> Value {
    type Entry = extern "rust-preserve-none" fn(
        Value,
        *mut Value,
        NumberTag,
        *mut u32,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
    ) -> Value;
    let entry: Entry = unsafe { std::mem::transmute(f) };
    let u = Value::UNDEFINED;
    entry(
        acc,
        regs,
        NumberTag::new(),
        ctx,
        u,
        u,
        u,
        u,
        u,
        u,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    )
}

#[cfg(target_arch = "aarch64")]
unsafe fn call_entry(f: *const u8, acc: Value, regs: *mut Value, ctx: *mut u32) -> Value {
    type Entry = extern "rust-preserve-none" fn(
        Value,
        *mut Value,
        NumberTag,
        *mut u32,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        Value,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
        f64,
    ) -> Value;
    let entry: Entry = unsafe { std::mem::transmute(f) };
    let u = Value::UNDEFINED;
    entry(
        acc,
        regs,
        NumberTag::new(),
        ctx,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        u,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
        0.0,
    )
}

/// Machine code for one entry into a bytecode function.
pub struct JitCode {
    mem: ExecMem,
    entry: usize,
    /// Bytes of hot code, from the start (excluding cold code).
    main_len: usize,
    /// The selected stencil per instruction (`None`: unreachable from the
    /// entry), for inspection and tests.
    pub stencils: Vec<Option<String>>,
    /// Where each VM register lived.
    pub homes: Vec<Home>,
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

/// Compilation options.
#[derive(Clone, Copy)]
struct Options {
    specialize: bool,
    register_homes: bool,
}

impl JitCode {
    /// Compile `insns` for entry at instruction `entry` with the exact types
    /// `actual` of the live values.
    fn compile(
        insns: &[Insn],
        entry: usize,
        actual: TypeState,
        widened: &HashSet<usize>,
        opts: Options,
    ) -> JitCode {
        let n = insns.len();
        let nregs = actual.regs.len();
        let actual = if opts.specialize {
            actual
        } else {
            TypeState::any(nregs)
        };
        let widened = opts.specialize.then_some(widened);

        // Pass 1: types without pinning; choose homes. Pass 2: types with
        // the FP-homed registers pinned as doubles. Pinning only widens
        // int32 to double, so it can't make a pinned register `Any`; if it
        // ever did, unpin it and retry.
        let unpinned = vec![false; nregs];
        let ins1 = infer(
            insns,
            entry,
            actual.clone(),
            &Spec {
                widened,
                pinned: &unpinned,
            },
        );
        let mut no_fp = vec![false; nregs];
        let (homes, ins) = loop {
            let homes = if opts.register_homes {
                assign_homes(insns, &ins1, nregs, &no_fp)
            } else {
                vec![Home::Mem; nregs]
            };
            let pinned: Vec<bool> = homes.iter().map(|h| matches!(h, Home::Fpr(_))).collect();
            let mut entry_state = actual.clone();
            for (r, &p) in pinned.iter().enumerate() {
                if p {
                    entry_state.regs[r] = Ty::F64;
                }
            }
            let ins = infer(
                insns,
                entry,
                entry_state,
                &Spec {
                    widened,
                    pinned: &pinned,
                },
            );
            let bad: Vec<usize> = (0..nregs)
                .filter(|&r| pinned[r] && ins.iter().flatten().any(|st| st.regs[r] != Ty::F64))
                .collect();
            if bad.is_empty() {
                break (homes, ins);
            }
            for r in bad {
                no_fp[r] = true;
            }
        };
        let pinned: Vec<bool> = homes.iter().map(|h| matches!(h, Home::Fpr(_))).collect();
        let spec = Spec {
            widened,
            pinned: &pinned,
        };
        let sel: Vec<Option<Sel>> = (0..n)
            .map(|i| ins[i].as_ref().map(|st| select(&insns[i], i, st, &spec)))
            .collect();

        // Hot code: entry pieces, then each reachable instruction in bytecode
        // order: its fall-through edge conversions, then (at its label) any
        // pieces it needs first, then its stencil. Jump edges needing
        // conversions get cold stubs; exits get cold spill-and-exit stubs.
        let mut hot: Vec<Piece> = Vec::new();
        let mut stubs: Vec<Vec<Piece>> = Vec::new();
        let mut exits: Vec<(usize, Vec<Piece>)> = Vec::new();
        let mut label_piece = vec![usize::MAX; n];
        let mut names: Vec<Option<String>> = vec![None; n];

        let entry_seq = entry_pieces(&actual, ins[entry].as_ref().unwrap(), &homes);
        let has_entry_seq = !entry_seq.is_empty();
        hot.extend(ending_at(entry_seq, Target::Label(entry)));

        for i in 0..n {
            let (Some(s), Some(st)) = (&sel[i], &ins[i]) else {
                continue;
            };
            let insn = &insns[i];
            if i > 0
                && let Some(prev) = &sel[i - 1]
                && insns[i - 1].falls_through()
            {
                hot.extend(edge_conversions(&prev.out, st, &homes));
            }
            let (pre, name) = stencil_for(insn, s, st, &homes);
            let jump = if insn.jumps() {
                let cs = edge_conversions(&s.out, ins[insn.target].as_ref().unwrap(), &homes);
                if cs.is_empty() {
                    Target::Label(insn.target)
                } else {
                    stubs.push(ending_at(cs, Target::Label(insn.target)));
                    Target::Stub(stubs.len() - 1)
                }
            } else {
                Target::Next
            };
            label_piece[i] = hot.len();
            hot.extend(pre);
            let main = Piece {
                imm: insn.imm,
                reg: insn.reg,
                jump,
                exit: i,
                ..Piece::new(&name, 0)
            };
            if main.uses("toi_exit") {
                assert_ne!(st.acc, Ty::F64, "exits need a boxed acc");
                let mut seq = spills(&homes);
                seq.push(Piece {
                    imm: insn.pc as i32,
                    ..Piece::new("exit", 0)
                });
                exits.push((i, seq));
            }
            hot.push(main);
            names[i] = Some(name);
        }

        // Positions. Hot pieces back to back; then cold code: edge stubs,
        // exit stubs, helpers; then trampolines.
        let mut pos = 0;
        let place = |pieces: &[Piece], pos: &mut usize| -> Vec<usize> {
            pieces
                .iter()
                .map(|p| {
                    let at = *pos;
                    *pos += p.len();
                    at
                })
                .collect()
        };
        let hot_at = place(&hot, &mut pos);
        let main_len = pos;
        let label = |i: usize| hot_at[label_piece[i]];
        let entry_at = if has_entry_seq { 0 } else { label(entry) };

        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let stub_ats: Vec<Vec<usize>> = stubs.iter().map(|s| place(s, &mut pos)).collect();
        let exit_ats: Vec<Vec<usize>> = exits.iter().map(|(_, s)| place(s, &mut pos)).collect();
        let exit_at: HashMap<usize, usize> = exits
            .iter()
            .zip(&exit_ats)
            .map(|((i, _), ats)| (*i, ats[0]))
            .collect();
        // Helpers, per hot piece: (piece index, helper name) -> offset.
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
        let all_stencils = hot
            .iter()
            .chain(stubs.iter().flatten())
            .chain(exits.iter().flat_map(|(_, s)| s))
            .map(|p| p.stencil)
            .chain(helper_list.iter().map(|&(_, h, _)| h));
        for s in all_stencils {
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
            Target::Stub(s) => stub_ats[s][0],
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
        let cold = stubs
            .iter()
            .zip(&stub_ats)
            .chain(exits.iter().map(|(_, s)| s).zip(&exit_ats));
        for (seq, ats) in cold {
            for (p, &at) in seq.iter().zip(ats) {
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

        mem.seal();
        JitCode {
            mem,
            entry: entry_at,
            main_len,
            stencils: names,
            homes,
        }
    }

    /// Run from the entry point over `regs`, starting with `acc`.
    ///
    /// # Safety
    /// `regs` must cover every register operand, and the code must have been
    /// compiled for the current types of `acc` and `regs`.
    unsafe fn enter(&self, acc: Value, regs: &mut [Value]) -> Outcome {
        let mut exit = NO_EXIT;
        let acc = unsafe {
            call_entry(
                self.mem.ptr.add(self.entry),
                acc,
                regs.as_mut_ptr(),
                &mut exit,
            )
        };
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
    opts: Options,
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
            opts: Options {
                specialize: true,
                register_homes: true,
            },
            widened: HashSet::new(),
            cached: None,
            last: None,
            stats: JitStats::default(),
        }
    }

    /// Use only generic stencils, as milestone 1 did.
    pub fn unspecialized(mut self) -> JitFunction {
        self.opts = Options {
            specialize: false,
            register_homes: false,
        };
        self
    }

    /// Keep every VM register in memory, as milestone 2 did.
    pub fn without_register_homes(mut self) -> JitFunction {
        self.opts.register_homes = false;
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
        JitCode::compile(&self.insns, entry, state, &self.widened, self.opts)
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
