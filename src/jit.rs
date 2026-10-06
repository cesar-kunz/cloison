//! Copy-and-patch baseline JIT, milestone 1 (see `docs/future-work.md`).
//!
//! Each bytecode instruction becomes a copy of its stencil
//! (`stencils/stencils.rs`) with the holes patched. No type specialization:
//! the stencils do what the interpreter's `ANY`-state handlers do, minus
//! dispatch. Layout of the generated code:
//!
//! ```text
//! [ main code: one stencil per instruction, in bytecode order        ]
//! [ cold code: each instruction's `__slow` helpers                   ]
//! [ trampolines: one per runtime function (`toi_rt_*`) referenced    ]
//! ```
//!
//! A stencil whose last instruction jumps to `toi_continue` falls through
//! into the next stencil with that jump trimmed. Calls to runtime functions
//! go through trampolines, so `rel32` / `CALL26` relocations only ever reach
//! inside JIT memory. On arm64, operand holes are `adrp`+`add` pairs whose
//! "address" is the operand value. `adrp` is page-relative with a ±4 GiB
//! range, so JIT memory is mapped below 4 GiB there.

use crate::bytecode::Op;
use crate::value::{NumberTag, Value};
use std::collections::HashMap;

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

fn stencil_name(op: Op) -> &'static str {
    match op {
        Op::Nop => "nop",
        Op::LdaSmi => "lda_smi",
        Op::Ldar => "ldar",
        Op::Star => "star",
        Op::Add => "add",
        Op::Inc => "inc",
        Op::JumpIfLess => "jump_if_less",
        Op::Jump => "jump",
        Op::AddSmi => "add_smi",
        Op::JumpIfLessSmi => "jump_if_less_smi",
        Op::Return => "return",
    }
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

// ---- Compilation ----

struct Insn {
    op: Op,
    /// `toi_hole_imm`: the immediate operand, if any.
    imm: i32,
    /// `toi_hole_reg`: the register operand, if any.
    reg: u8,
    /// Instruction index of the jump target, if any.
    target: usize,
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
                Op::Ldar | Op::Star | Op::Add => reg = code[pc + 1],
                Op::JumpIfLess => (reg, target) = (code[pc + 1], index_of(pc, imm_at(pc + 2))),
                Op::JumpIfLessSmi => (imm, target) = (imm_at(pc + 1), index_of(pc, imm_at(pc + 5))),
                Op::Jump => target = index_of(pc, imm_at(pc + 1)),
                Op::Nop | Op::Inc | Op::Return => {}
            }
            Insn {
                op,
                imm,
                reg,
                target,
            }
        })
        .collect()
}

/// Compiled code for one bytecode function.
pub struct JitCode {
    mem: ExecMem,
    /// Bytes of main code (excluding cold code and trampolines).
    main_len: usize,
}

type Entry = extern "rust-preserve-none" fn(Value, *mut Value, NumberTag) -> Value;

impl JitCode {
    /// Compile `code`. Panics on malformed bytecode.
    pub fn compile(code: &[u8]) -> JitCode {
        let insns = decode(code);
        let n = insns.len();
        assert!(n > 0, "empty bytecode");

        // Layout. Main code first: each stencil, minus its trailing jump to
        // `toi_continue` (the next stencil follows directly).
        let mains: Vec<&Stencil> = insns.iter().map(|i| stencil(stencil_name(i.op))).collect();
        let mut main_off = Vec::with_capacity(n + 1);
        let mut main_len = Vec::with_capacity(n);
        let mut pos = 0;
        for (i, s) in mains.iter().enumerate() {
            let trim = i + 1 < n && arch::ends_with_continue(s.code, s.relocs);
            let len = s.code.len() - if trim { arch::TRAILING_JUMP } else { 0 };
            main_off.push(pos);
            main_len.push(len);
            pos += len;
        }
        main_off.push(pos);
        let main_end = pos;

        // Cold code: each instruction's helpers.
        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let mut helper_off: HashMap<(usize, &str), usize> = HashMap::new();
        let mut helper_list = Vec::new();
        for (i, s) in mains.iter().enumerate() {
            for h in helpers(s.name) {
                helper_off.insert((i, h.name), pos);
                helper_list.push((i, h, pos));
                pos += h.code.len();
            }
        }

        // Trampolines for the runtime functions referenced anywhere.
        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let mut tramp_off: HashMap<&str, usize> = HashMap::new();
        let all = mains
            .iter()
            .copied()
            .chain(helper_list.iter().map(|&(_, h, _)| h));
        for s in all {
            for r in s.relocs.iter().filter(|r| r.symbol.starts_with("toi_rt_")) {
                tramp_off.entry(r.symbol).or_insert_with(|| {
                    let at = pos;
                    pos += arch::TRAMPOLINE_SIZE;
                    at
                });
            }
        }

        let mut mem = ExecMem::new(pos);
        let base = mem.ptr as u64;
        let buf = mem.bytes();

        for (&name, &at) in &tramp_off {
            arch::write_trampoline(&mut buf[at..], runtime_function(name) as u64);
        }

        // Copy one stencil instance and patch it for instruction `i`.
        let mut emit = |i: usize, s: &Stencil, at: usize, len: usize| {
            buf[at..at + len].copy_from_slice(&s.code[..len]);
            let insn = &insns[i];
            for r in s.relocs.iter().filter(|r| (r.offset as usize) < len) {
                let value = match r.symbol {
                    "toi_hole_imm" => insn.imm as u32 as u64,
                    "toi_hole_reg" => insn.reg as u64 * size_of::<Value>() as u64,
                    "toi_continue" => base + main_off[i + 1] as u64,
                    "toi_jump_target" => base + main_off[insn.target] as u64,
                    sym if sym.starts_with("toi_rt_") => base + tramp_off[sym] as u64,
                    sym if sym.starts_with('@') => {
                        let off = helper_off.get(&(i, &sym[1..])).unwrap_or_else(|| {
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
        for (i, s) in mains.iter().enumerate() {
            emit(i, s, main_off[i], main_len[i]);
        }
        for &(i, h, at) in &helper_list {
            emit(i, h, at, h.code.len());
        }

        mem.seal();
        JitCode {
            mem,
            main_len: main_end,
        }
    }

    /// Run over the register file `regs` and return the accumulator at
    /// `Return`. The accumulator starts as `undefined`.
    ///
    /// # Safety
    /// As [`crate::interp::run`]: register operands must be `< regs.len()`,
    /// and every path must end in `Return`.
    pub unsafe fn run(&self, regs: &mut [Value]) -> Value {
        let entry: Entry = unsafe { std::mem::transmute(self.mem.ptr) };
        entry(Value::UNDEFINED, regs.as_mut_ptr(), NumberTag::new())
    }

    /// The main (hot) machine code, for inspection.
    pub fn main_code(&self) -> &[u8] {
        unsafe { std::slice::from_raw_parts(self.mem.ptr, self.main_len) }
    }

    /// Address of the generated code.
    pub fn address(&self) -> *const u8 {
        self.mem.ptr
    }
}

/// Compile `code` and run it once. See [`JitCode::run`].
///
/// # Safety
/// As [`crate::interp::run`].
pub unsafe fn run(code: &[u8], regs: &mut [Value]) -> Value {
    unsafe { JitCode::compile(code).run(regs) }
}
