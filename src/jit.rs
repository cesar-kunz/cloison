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
//!    inference runs again), in a GPR slot as an unboxed int32 (if it's only
//!    ever int32), in a GPR slot boxed, or in memory. Registers not live at
//!    entry start as `Bot`, and may be pinned only if definitely assigned
//!    before any spill ([`pinnable`]). Slots go
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
//! 5. [`Fusion`]: common sequences on slot registers (`ldar r; add_smi k;
//!    star r`, the loop tail `ldar i; inc; star i; jlt_smi n`, …) become a
//!    single fused stencil that bypasses `acc`, guided by accumulator
//!    liveness ([`acc_live_out`]).
//!
//! Layout of the generated code:
//!
//! ```text
//! [ hot: entry (load cached registers, convert); per instruction:        ]
//! [      fall-through conversions, then (at its label) e.g. spills       ]
//! [      before `return`, then the instruction's stencil                 ]
//! [ cold: edge stubs, exit stubs (spill, then exit), `__slow` helpers    ]
//! [ runtime-call wrappers: one per `toi_rt_*` referenced, saving the     ]
//! [      slot registers this region uses                                 ]
//! ```
//!
//! On arm64 most stencils come from family templates: `build.rs` stores one
//! template per family of slot variants plus the positions of its register
//! fields, and [`Instance::copy_to`] writes the slot registers in while
//! copying (see `compact_arm64` in `build.rs`).
//!
//! A stencil whose last instruction jumps to `toi_continue` falls through
//! into the next piece with that jump trimmed. Calls to runtime functions
//! go to wrappers in JIT memory ([`arch::runtime_wrapper`]), so `rel32` /
//! `CALL26` relocations only ever reach inside JIT memory. The wrappers save
//! only the slot registers the region uses, instead of every stencil
//! saving all of them around a C call. Operand holes are absolute
//! relocations (x86-64: immediates and displacements, some folded into the
//! instruction through inline asm; arm64: `movz`/`movk`), so JIT memory can
//! be anywhere.

use crate::bytecode::Op;
use crate::value::{NumberTag, Value};
use std::collections::{HashMap, HashSet};

/// What a stencil relocation refers to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Sym {
    HoleImm,
    HoleReg,
    Continue,
    JumpTarget,
    Exit,
    /// The stencil's own `__slow` helper.
    Slow,
    /// A runtime function, `toi_rt_*`: an index into `RT_NAMES`.
    Rt(u8),
}

/// A relocation in a stencil, from `build.rs`: offset of the patched field,
/// ELF relocation type, symbol, addend.
pub(crate) struct R(u16, u16, Sym, i32);

pub(crate) struct Stencil {
    /// The stencil's name, or for an arm64 family template its key, with
    /// `{}` for each slot number (`addr_ii_u{}_u{}`).
    name: &'static str,
    /// Whether `<name>__slow` exists.
    slow: bool,
    code: &'static [u8],
    relocs: &'static [R],
    /// For an arm64 family template, its register fields.
    template: Option<&'static Template>,
}

/// What makes a stencil a family template (arm64; see `compact_arm64` in
/// `build.rs`).
pub(crate) struct Template {
    /// (instruction offset, bit position, parameter): the JIT writes the
    /// parameter's register into the 5-bit field.
    fields: &'static [(u16, u8, u8)],
    /// The slot numbers of the family members this template covers (a
    /// family may need several templates).
    covers: &'static [[u8; 2]],
}

include!(concat!(env!("OUT_DIR"), "/stencils.rs"));

/// Lookup tables: concrete stencils by name, and family templates by
/// (family key, slot numbers).
struct Index {
    by_name: HashMap<&'static str, &'static Stencil>,
    by_family: HashMap<(&'static str, [u8; 2]), &'static Stencil>,
}

fn index() -> &'static Index {
    static INDEX: std::sync::OnceLock<Index> = std::sync::OnceLock::new();
    INDEX.get_or_init(|| {
        let mut ix = Index {
            by_name: HashMap::new(),
            by_family: HashMap::new(),
        };
        for s in STENCILS {
            match s.template {
                None => {
                    ix.by_name.insert(s.name, s);
                }
                Some(t) => {
                    for &slots in t.covers {
                        ix.by_family.insert((s.name, slots), s);
                    }
                }
            }
        }
        ix
    })
}

/// Split a stencil name into its family key and slot parameters; see
/// `slot_params` in `build.rs`, which this must match.
fn slot_params(name: &str) -> (String, Vec<(char, u8)>) {
    let mut key = String::with_capacity(name.len());
    let mut params = Vec::new();
    for (i, tok) in name.split('_').enumerate() {
        if i > 0 {
            key.push('_');
        }
        let mut cs = tok.chars();
        match (cs.next(), cs.as_str().parse::<u8>()) {
            (Some(c @ ('g' | 'u' | 'f')), Ok(k)) if i > 0 && !cs.as_str().is_empty() => {
                key.push(c);
                key.push_str("{}");
                params.push((c, k));
            }
            _ => key.push_str(tok),
        }
    }
    (key, params)
}

/// A stencil to copy, with the registers to patch into a family template's
/// fields.
#[derive(Clone, Copy)]
struct Instance {
    stencil: &'static Stencil,
    regs: [u8; 2],
}

impl Instance {
    /// The stencil named `name`: a concrete stencil, or an instance of the
    /// family template it belongs to (arm64).
    fn find(name: &str) -> Option<Instance> {
        let ix = index();
        if let Some(&stencil) = ix.by_name.get(name) {
            return Some(Instance {
                stencil,
                regs: [0; 2],
            });
        }
        let (key, params) = slot_params(name);
        let (mut slots, mut regs) = ([0; 2], [0; 2]);
        for (i, &p) in params.iter().enumerate().take(2) {
            slots[i] = p.1;
            regs[i] = arch::slot_reg(p);
        }
        let &stencil = ix.by_family.get(&(key.as_str(), slots))?;
        Some(Instance { stencil, regs })
    }

    fn get(name: &str) -> Instance {
        Instance::find(name).unwrap_or_else(|| panic!("no stencil `{name}`"))
    }

    /// Copy the stencil's first `len` bytes to `buf` and patch in the
    /// registers of a family template.
    fn copy_to(&self, buf: &mut [u8], len: usize) {
        buf[..len].copy_from_slice(&self.stencil.code[..len]);
        let Some(template) = self.stencil.template else {
            return;
        };
        for &(at, shift, p) in template.fields {
            let at = at as usize;
            let w = u32::from_le_bytes(buf[at..at + 4].try_into().unwrap());
            let w = (w & !(31 << shift)) | (self.regs[p as usize] as u32) << shift;
            buf[at..at + 4].copy_from_slice(&w.to_le_bytes());
        }
    }
}

fn has_stencil(name: &str) -> bool {
    Instance::find(name).is_some()
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
    pub const CODE_ALIGN: usize = 16;

    /// Whether the stencil ends with `jmp toi_continue`.
    pub fn ends_with_continue(code: &[u8], relocs: &[super::R]) -> bool {
        let n = code.len();
        n >= 5
            && code[n - 5] == 0xE9
            && relocs
                .iter()
                .any(|r| r.2 == super::Sym::Continue && r.0 as usize == n - 4)
    }

    /// x86-64 stencils have no family templates (register fields aren't
    /// patchable in place: encodings vary in length).
    pub fn slot_reg(_: (char, u8)) -> u8 {
        unreachable!("no register-patched stencils on x86-64")
    }

    /// The register number of GPR slot `k` (rdi rsi rdx rcx r8 r9), if the
    /// C ABI doesn't preserve it, so a runtime-call wrapper must. On x86-64
    /// all GPR slots are caller-saved.
    pub fn caller_saved_gpr_slot(k: u8) -> Option<u8> {
        Some([7, 6, 2, 1, 8, 9][k as usize])
    }

    /// A runtime-call wrapper (see `rt_call!` in `stencils/stencils.rs`):
    /// called with the arguments in rax and r11, it saves GPRs `gprs` and
    /// xmm registers `fprs`, calls the C function `target`, restores them
    /// and returns the result in rax. Position-independent.
    ///
    /// Entered with rsp ≡ 8 (mod 16), as from any call at an aligned site.
    pub fn runtime_wrapper(target: u64, gprs: &[u8], fprs: &[u8]) -> Vec<u8> {
        let mut c = Vec::new();
        for &r in gprs {
            if r >= 8 {
                c.push(0x41);
            }
            c.push(0x50 + (r & 7)); // push r
        }
        // Spill area for xmm registers, padded so the call site is aligned.
        let mut frame = 8 * fprs.len();
        if !(8 + 8 * gprs.len() + frame).is_multiple_of(16) {
            frame += 8;
        }
        if frame > 0 {
            c.extend([0x48, 0x83, 0xEC, frame as u8]); // sub rsp, frame
        }
        let movsd = |c: &mut Vec<u8>, op: u8, x: u8, i: usize| {
            // movsd [rsp + 8i], xmm / movsd xmm, [rsp + 8i]
            c.extend([0xF2, 0x0F, op, 0x44 | (x << 3), 0x24, (8 * i) as u8]);
        };
        for (i, &x) in fprs.iter().enumerate() {
            movsd(&mut c, 0x11, x, i);
        }
        c.extend([0x48, 0x89, 0xC7]); // mov rdi, rax
        c.extend([0x4C, 0x89, 0xDE]); // mov rsi, r11
        c.extend([0x48, 0xB8]); // movabs rax, target
        c.extend(target.to_le_bytes());
        c.extend([0xFF, 0xD0]); // call rax
        for (i, &x) in fprs.iter().enumerate() {
            movsd(&mut c, 0x10, x, i);
        }
        if frame > 0 {
            c.extend([0x48, 0x83, 0xC4, frame as u8]); // add rsp, frame
        }
        for &r in gprs.iter().rev() {
            if r >= 8 {
                c.push(0x41);
            }
            c.push(0x58 + (r & 7)); // pop r
        }
        c.push(0xC3); // ret
        c
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
    const R_AARCH64_MOVW_UABS_G0_NC: u32 = 264;
    const R_AARCH64_MOVW_UABS_G1_NC: u32 = 266;
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
    pub const CODE_ALIGN: usize = 16;

    pub fn ends_with_continue(code: &[u8], relocs: &[super::R]) -> bool {
        let n = code.len();
        n >= 4
            && relocs.iter().any(|r| {
                r.2 == super::Sym::Continue
                    && r.0 as usize == n - 4
                    && r.1 as u32 == R_AARCH64_JUMP26
            })
    }

    /// The register of a slot parameter, as `slot_params` names them: GPR
    /// slots 0–4 are x24–x28, 5–11 are x0–x6; FP slot k is d(k+1). Must
    /// match `arm64_slot_reg` in `build.rs`.
    pub fn slot_reg((class, k): (char, u8)) -> u8 {
        match class {
            'f' => k + 1,
            _ if k < 5 => 24 + k,
            _ => k - 5,
        }
    }

    /// The register number of GPR slot `k`, if the C ABI doesn't preserve
    /// it, so a runtime-call wrapper must. Slots 0–4 are x24–x28,
    /// callee-saved; slots 5–11 are x0–x6.
    pub fn caller_saved_gpr_slot(k: u8) -> Option<u8> {
        (k >= 5).then(|| k - 5)
    }

    /// A runtime-call wrapper (see `rt_call!` in `stencils/stencils.rs`):
    /// called with `bl` and the arguments in x8 and x9, it saves GPRs `gprs`
    /// and FP registers `fprs` (d0–d7), calls the C function `target`,
    /// restores them and returns the result in x8. Position-independent; the
    /// target address is a literal after the code.
    pub fn runtime_wrapper(target: u64, gprs: &[u8], fprs: &[u8]) -> Vec<u8> {
        let mut c: Vec<u32> = Vec::new();
        c.push(0xA9BF_7BFD); // stp x29, x30, [sp, #-16]!
        c.push(0x9100_03FD); // mov x29, sp
        let n = gprs.len() + fprs.len();
        let frame = (8 * n).next_multiple_of(16) as u32;
        if frame > 0 {
            c.push(0xD100_03FF | frame << 10); // sub sp, sp, #frame
        }
        // str / ldr x or d register at [sp, #8i]
        let slot = |op: u32, r: u8, i: usize| op | (i as u32) << 10 | (31 << 5) | r as u32;
        let saves = gprs.iter().map(|&r| (0xF900_0000, 0xF940_0000, r));
        let saves = saves.chain(fprs.iter().map(|&r| (0xFD00_0000, 0xFD40_0000, r)));
        let saves: Vec<_> = saves.collect();
        for (i, &(st, _, r)) in saves.iter().enumerate() {
            c.push(slot(st, r, i));
        }
        c.push(0xAA08_03E0); // mov x0, x8
        c.push(0xAA09_03E1); // mov x1, x9
        let ldr_at = c.len();
        c.push(0); // ldr x16, literal (patched below)
        c.push(0xD63F_0200); // blr x16
        c.push(0xAA00_03E8); // mov x8, x0
        for (i, &(_, ld, r)) in saves.iter().enumerate() {
            c.push(slot(ld, r, i));
        }
        if frame > 0 {
            c.push(0x9100_03FF | frame << 10); // add sp, sp, #frame
        }
        c.push(0xA8C1_7BFD); // ldp x29, x30, [sp], #16
        c.push(0xD65F_03C0); // ret
        let lit = c.len();
        c[ldr_at] = 0x5800_0010 | ((lit - ldr_at) as u32) << 5;
        let mut bytes: Vec<u8> = c.iter().flat_map(|i| i.to_le_bytes()).collect();
        bytes.extend(target.to_le_bytes());
        bytes
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
            // movz / movk: imm16 at [20:5], bits 0–15 / 16–31 of the value.
            R_AARCH64_MOVW_UABS_G0_NC | R_AARCH64_MOVW_UABS_G1_NC => {
                let shift = if r_type == R_AARCH64_MOVW_UABS_G0_NC {
                    0
                } else {
                    16
                };
                let imm16 = ((v >> shift) & 0xFFFF) as u32;
                write(buf, (read(buf) & !(0xFFFF << 5)) | imm16 << 5);
            }
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
        let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, prot, flags, -1, 0) };
        assert!(ptr != libc::MAP_FAILED, "mmap failed");
        ExecMem {
            ptr: ptr.cast(),
            len,
        }
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
        // Generic code keeps `acc` boxed, so `lda_smi`, not `lda_smi_i`.
        let case = if base == "lda_smi" { "" } else { case };
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
            ("lda_smi", "i")
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
    /// GPR slot `k`, an unboxed int32 (zero-extended). The register is only
    /// ever `I32` in the region.
    Int(u8),
    /// FP slot `k`, an unboxed double. The register is pinned as `F64`.
    Fpr(u8),
}

impl Home {
    fn loc(self) -> String {
        match self {
            Home::Mem => "m".into(),
            Home::Gpr(k) => format!("g{k}"),
            Home::Int(k) => format!("u{k}"),
            Home::Fpr(k) => format!("f{k}"),
        }
    }
}

/// Assign homes by static use count. Registers whose type is only ever
/// int32 or double (and at least once double) get FP slots; registers only
/// ever int32 get GPR slots unboxed (`Int`); other used registers get GPR
/// slots boxed; the rest stay in memory. `no_fp` / `no_int` exclude
/// registers from pinning, as does `!pin_ok` (see [`pinnable`]).
fn assign_homes(
    insns: &[Insn],
    ins: &[Option<TypeState>],
    nregs: usize,
    no_fp: &[bool],
    no_int: &[bool],
    pin_ok: &[bool],
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
        if join[r] == Ty::F64 && !no_fp[r] && pin_ok[r] && fp < FP_SLOTS {
            homes[r] = Home::Fpr(fp as u8);
            fp += 1;
        }
    }
    for &r in &by_uses {
        if homes[r] == Home::Mem && gpr < GPR_SLOTS {
            homes[r] = if join[r] == Ty::I32 && !no_int[r] && pin_ok[r] {
                Home::Int(gpr as u8)
            } else {
                Home::Gpr(gpr as u8)
            };
            gpr += 1;
        }
    }
    homes
}

/// Instructions reachable from `entry`.
fn reachable(insns: &[Insn], entry: usize) -> Vec<bool> {
    let mut seen = vec![false; insns.len()];
    let mut work = vec![entry];
    while let Some(i) = work.pop() {
        if i >= insns.len() || std::mem::replace(&mut seen[i], true) {
            continue;
        }
        if insns[i].falls_through() {
            work.push(i + 1);
        }
        if insns[i].jumps() {
            work.push(insns[i].target);
        }
    }
    seen
}

fn succs(insns: &[Insn], i: usize) -> impl Iterator<Item = usize> {
    let insn = &insns[i];
    [
        insn.falls_through().then_some(i + 1),
        insn.jumps().then_some(insn.target),
    ]
    .into_iter()
    .flatten()
}

/// Registers live into each reachable instruction (backward dataflow):
/// `ldar`, `add`, `jlt` read their register, `star` writes it.
fn reg_live_in(insns: &[Insn], reach: &[bool], nregs: usize) -> Vec<Vec<bool>> {
    let n = insns.len();
    let mut live_in = vec![vec![false; nregs]; n];
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev().filter(|&i| reach[i]) {
            let insn = &insns[i];
            let mut live = vec![false; nregs];
            for s in succs(insns, i).filter(|&s| s < n) {
                for (l, &x) in live.iter_mut().zip(&live_in[s]) {
                    *l |= x;
                }
            }
            match insn.op {
                Op::Star => live[insn.reg as usize] = false,
                Op::Ldar | Op::Add | Op::JumpIfLess => live[insn.reg as usize] = true,
                _ => {}
            }
            if live != live_in[i] {
                live_in[i] = live;
                changed = true;
            }
        }
    }
    live_in
}

/// Registers that may be pinned (kept unboxed in a slot). A pinned register
/// can't hold an arbitrary value, so one that isn't live at entry (and so
/// isn't loaded) must be written on every path before any point where the
/// slots are spilled: a possible int32 overflow exit, or `return`.
/// Otherwise the spill would overwrite its untouched value in memory.
fn pinnable(
    insns: &[Insn],
    entry: usize,
    reach: &[bool],
    live_at_entry: &[bool],
    ins: &[Option<TypeState>],
    widened: &HashSet<usize>,
) -> Vec<bool> {
    let n = insns.len();
    let nregs = live_at_entry.len();
    // Definitely assigned on entry to each instruction (forward, must).
    let mut assigned: Vec<Option<Vec<bool>>> = vec![None; n];
    assigned[entry] = Some(live_at_entry.to_vec());
    let mut work = vec![entry];
    while let Some(i) = work.pop() {
        let mut out = assigned[i].clone().unwrap();
        if insns[i].op == Op::Star {
            out[insns[i].reg as usize] = true;
        }
        for s in succs(insns, i).filter(|&s| s < n) {
            let changed = match &mut assigned[s] {
                Some(a) => {
                    let mut changed = false;
                    for (x, &o) in a.iter_mut().zip(&out) {
                        if *x && !o {
                            *x = false;
                            changed = true;
                        }
                    }
                    changed
                }
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
    let may_spill = |i: usize| {
        let Some(st) = &ins[i] else { return false };
        let reg = st.regs.get(insns[i].reg as usize).copied();
        match insns[i].op {
            Op::Return => true,
            Op::AddSmi | Op::Inc => st.acc == Ty::I32 && !widened.contains(&i),
            Op::Add => st.acc == Ty::I32 && reg == Some(Ty::I32) && !widened.contains(&i),
            _ => false,
        }
    };
    (0..nregs)
        .map(|r| {
            live_at_entry[r]
                || (0..n)
                    .filter(|&i| reach[i] && may_spill(i))
                    .all(|i| assigned[i].as_ref().is_some_and(|a| a[r]))
        })
        .collect()
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
    stencil: Instance,
    /// Its `__slow` helper, if any (copied into the cold area).
    slow: Option<Instance>,
    imm: i32,
    reg: u8,
    cont: Target,
    jump: Target,
    /// Instruction whose exit stub `toi_exit` goes to.
    exit: usize,
}

impl Piece {
    /// The stencil `name` with immediate `imm`. On arm64, use its `_k16`
    /// variant (one `movz` instead of `movz` + `movk`) when it has one and
    /// the immediate fits.
    fn with_imm(name: &str, imm: i32) -> Piece {
        let k16 = format!("{name}_k16");
        let fits = cfg!(target_arch = "aarch64") && (0..=0xFFFF).contains(&imm);
        let name = if fits && has_stencil(&k16) {
            k16.as_str()
        } else {
            name
        };
        Piece {
            imm,
            ..Piece::new(name, 0)
        }
    }

    fn new(name: &str, reg: u8) -> Piece {
        let stencil = Instance::get(name);
        let slow = stencil
            .stencil
            .slow
            .then(|| Instance::get(&format!("{name}__slow")));
        Piece {
            stencil,
            slow,
            imm: 0,
            reg,
            cont: Target::Next,
            jump: Target::Next,
            exit: usize::MAX,
        }
    }

    fn trims(&self) -> bool {
        let s = self.stencil.stencil;
        matches!(self.cont, Target::Next) && arch::ends_with_continue(s.code, s.relocs)
    }

    fn len(&self) -> usize {
        self.stencil.stencil.code.len() - if self.trims() { arch::TRAILING_JUMP } else { 0 }
    }

    fn uses(&self, symbol: Sym) -> bool {
        self.stencil.stencil.relocs.iter().any(|r| r.2 == symbol)
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
        (Ty::I32, Ty::Any) => out.push(Piece::new("box_iacc", 0)),
        _ => {}
    }
    for (r, (&a, &b)) in from.regs.iter().zip(&to.regs).enumerate() {
        if a == Ty::I32 && b == Ty::F64 && matches!(homes[r], Home::Mem | Home::Gpr(_)) {
            out.push(Piece::new(&format!("conv_{}_f", homes[r].loc()), r as u8));
        }
    }
    out
}

/// Entry: load cached registers from the register file (unboxing pinned
/// ones), then convert to the entry instruction's in-state. `actual` is the
/// exact entry state (`acc` arrives boxed); `actual_live` has dead
/// registers as `Bot`. Pinned registers that aren't live at entry aren't
/// loaded (see [`pinnable`]); boxed ones always are, so spills stay exact.
fn entry_pieces(
    actual: &TypeState,
    actual_live: &TypeState,
    to: &TypeState,
    homes: &[Home],
    live_at_entry: &[bool],
) -> Vec<Piece> {
    let mut out = Vec::new();
    for (r, &h) in homes.iter().enumerate() {
        match h {
            Home::Mem => {}
            Home::Gpr(k) => out.push(Piece::new(&format!("enter_g{k}"), r as u8)),
            _ if !live_at_entry[r] => {}
            Home::Int(k) => {
                assert_eq!(actual.regs[r], Ty::I32, "r{r} pinned as int32");
                out.push(Piece::new(&format!("enter_u{k}"), r as u8));
            }
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
    let mut regs_only = actual_live.clone();
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
            Home::Int(k) => Some(Piece::new(&format!("spill_u{k}"), r as u8)),
            Home::Fpr(k) => Some(Piece::new(&format!("spill_f{k}"), r as u8)),
        })
        .collect()
}

/// The piece that boxes an accumulator of type `acc`, if it isn't boxed:
/// `box_facc` for a double in `facc`, `box_iacc` for an int32 (whose high
/// bits may be 0 instead of the tag).
fn box_acc_piece(acc: Ty) -> Option<Piece> {
    match acc {
        Ty::F64 => Some(Piece::new("box_facc", 0)),
        Ty::I32 => Some(Piece::new("box_iacc", 0)),
        _ => None,
    }
}

/// The stencil name for instruction `insn`, and pieces to run before it.
fn stencil_for(insn: &Insn, sel: &Sel, st: &TypeState, homes: &[Home]) -> (Vec<Piece>, String) {
    let mut pre = Vec::new();
    let home = homes.get(insn.reg as usize).copied().unwrap_or(Home::Mem);
    let loc = home.loc();
    // Generic stencils (and `return`) take a boxed acc.
    let box_acc = |pre: &mut Vec<Piece>| pre.extend(box_acc_piece(st.acc));
    let name = match (sel.base, home) {
        ("ldar", Home::Fpr(k)) => format!("ldar_f{k}"),
        ("ldar", _) if sel.case == "f" => format!("ldar_{loc}_f"),
        ("ldar", _) => format!("ldar_{loc}"),
        ("star", Home::Fpr(k)) if sel.case == "f" => format!("star_f{k}"),
        ("star", Home::Fpr(k)) => format!("star_i_f{k}"),
        ("star", _) if sel.case == "f" => format!("star_{loc}_f"),
        ("star", Home::Mem | Home::Gpr(_)) if sel.case == "i" => format!("star_{loc}_i"),
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

// ---- Fused stencils ----

/// Whether the accumulator is live after each instruction (backward
/// dataflow over reachable instructions). An instruction reads `acc` unless
/// it is `ldar` / `lda_smi`, which overwrite it, or `nop` / `jump`, which
/// pass it through.
fn acc_live_out(insns: &[Insn], ins: &[Option<TypeState>]) -> Vec<bool> {
    let n = insns.len();
    let reads = |op: Op| !matches!(op, Op::Ldar | Op::LdaSmi | Op::Nop | Op::Jump);
    let kills = |op: Op| matches!(op, Op::Ldar | Op::LdaSmi);
    let mut live_in = vec![false; n];
    let mut live_out = vec![false; n];
    let mut changed = true;
    while changed {
        changed = false;
        for i in (0..n).rev() {
            if ins[i].is_none() {
                continue;
            }
            let insn = &insns[i];
            let succs = [
                insn.falls_through().then_some(i + 1),
                insn.jumps().then_some(insn.target),
            ];
            let out = succs.into_iter().flatten().any(|s| live_in[s]);
            let inn = reads(insn.op) || (!kills(insn.op) && out);
            if out != live_out[i] || inn != live_in[i] {
                (live_out[i], live_in[i]) = (out, inn);
                changed = true;
            }
        }
    }
    live_out
}

/// One stencil of a fused group.
struct FusedStep {
    name: String,
    imm: i32,
    /// The bytecode instruction an exit from this step happens before.
    insn: usize,
    /// For in-place ops on unboxed slots, which write the wrapped result
    /// before checking overflow: the stencil (and immediate) that undoes it
    /// in the exit stub.
    undo: Option<(String, i32)>,
}

/// A matched fused group: one or more stencils replacing `len` bytecode
/// instructions.
struct Fused {
    len: usize,
    steps: Vec<FusedStep>,
    /// `ldar` stencil for the slot holding the group's result, to
    /// rematerialize `acc` after the group or in an exit stub (where that
    /// slot holds the interpreter's `acc` at the exiting instruction).
    ldar: String,
    acc_live_after: bool,
}

impl Fused {
    fn single(
        len: usize,
        name: String,
        imm: i32,
        ldar: String,
        acc_live_after: bool,
        undo: Option<(String, i32)>,
        insn: usize,
    ) -> Fused {
        Fused {
            len,
            steps: vec![FusedStep {
                name,
                imm,
                insn,
                undo,
            }],
            ldar,
            acc_live_after,
        }
    }
}

/// Matches fused-stencil patterns (see the fused section of
/// `stencils/stencils.rs`). A group needs its operands in slots with the
/// right types, no jump target (or the entry) inside it, and, for the loop
/// tail, a dead accumulator on both branches.
struct Fusion<'a> {
    insns: &'a [Insn],
    ins: &'a [Option<TypeState>],
    homes: &'a [Home],
    /// `None` when not specializing: no fusion.
    widened: Option<&'a HashSet<usize>>,
    live_out: &'a [bool],
    is_target: &'a [bool],
}

impl Fusion<'_> {
    fn at(&self, i: usize) -> Option<Fused> {
        let widened = self.widened?;
        if let Some(f) = self.add_chain(i, widened) {
            return Some(f);
        }
        let insns = self.insns;
        let group = |len: usize| {
            i + len <= insns.len()
                && (i + 1..i + len).all(|j| !self.is_target[j] && self.ins[j].is_some())
        };
        if !group(3)
            || insns[i].op != Op::Ldar
            || insns[i + 2].op != Op::Star
            || insns[i + 2].reg != insns[i].reg
        {
            return None;
        }
        let r = insns[i].reg as usize;
        let st = self.ins[i].as_ref()?;
        let mid = &insns[i + 1];
        let int_ok = st.regs[r] == Ty::I32 && !widened.contains(&(i + 1));
        let dbl = st.regs[r] == Ty::F64;
        let home = self.homes[r];
        if home == Home::Mem {
            return None;
        }
        let ldar = format!("ldar_{}", home.loc());
        let fused = |len: usize, name: String, imm: i32| {
            let acc_live_after = self.live_out[i + len - 1];
            Some(Fused::single(
                len,
                name,
                imm,
                ldar.clone(),
                acc_live_after,
                None,
                i + 1,
            ))
        };
        let undone = |f: Option<Fused>, undo: String, imm: i32| {
            f.map(|mut f| {
                f.steps[0].undo = Some((undo, imm));
                f
            })
        };

        // Loop tail: ldar r; inc; star r; jlt_smi k | jlt r2.
        if mid.op == Op::Inc && int_ok && group(4) && !self.live_out[i + 3] {
            let tail = &insns[i + 3];
            let bound = match tail.op {
                Op::JumpIfLess => {
                    let r2 = tail.reg as usize;
                    (self.ins[i + 3].as_ref()?.regs[r2] == Ty::I32).then_some(self.homes[r2])
                }
                _ => None,
            };
            match (home, tail.op, bound) {
                (Home::Gpr(a), Op::JumpIfLessSmi, _) => {
                    return fused(4, format!("incjlt_g{a}"), tail.imm);
                }
                (Home::Gpr(a), Op::JumpIfLess, Some(Home::Gpr(b))) => {
                    return fused(4, format!("incjlt_g{a}_g{b}"), 0);
                }
                (Home::Int(a), Op::JumpIfLessSmi, _) => {
                    let f = fused(4, format!("incjlt_u{a}"), tail.imm);
                    return undone(f, format!("undo_addi_u{a}"), 1);
                }
                (Home::Int(a), Op::JumpIfLess, Some(Home::Int(b))) => {
                    let f = fused(4, format!("incjlt_u{a}_u{b}"), 0);
                    return undone(f, format!("undo_addi_u{a}"), 1);
                }
                _ => {}
            }
        }

        match (mid.op, home) {
            // ldar r; add_smi k | inc; star r
            (Op::AddSmi | Op::Inc, Home::Gpr(a)) if int_ok => {
                fused(3, format!("addi_g{a}"), mid.imm)
            }
            (Op::AddSmi | Op::Inc, Home::Int(a)) if int_ok => undone(
                fused(3, format!("addi_u{a}"), mid.imm),
                format!("undo_addi_u{a}"),
                mid.imm,
            ),
            (Op::AddSmi | Op::Inc, Home::Fpr(a)) if dbl => fused(3, format!("addi_f{a}"), mid.imm),
            _ => None,
        }
    }
}

impl Fusion<'_> {
    /// `ldar a; add b1; …; add bn; star c` (n ≥ 1) as `c = a` (a slot move,
    /// skipped when `c == a`) followed by in-place `c += b_k` stencils.
    ///
    /// If add k overflows, its stencil exits with the wrapped sum in `c`; the
    /// exit stub undoes it, so `c` holds `a + b1 + … + b(k-1)`, the
    /// interpreter's `acc` before add k, and rematerializes `acc` from it.
    /// `c` itself is dead there (the pending `star c` overwrites it), which
    /// requires that no `b_k` is `c`: that add would read `c`'s original
    /// value. With `c` in an FP slot the chain is computed in double from the
    /// start, which is exact for int32 operands too; the result must then be
    /// typed double, so a live `acc` stays consistent.
    fn add_chain(&self, i: usize, widened: &HashSet<usize>) -> Option<Fused> {
        let insns = self.insns;
        if insns[i].op != Op::Ldar {
            return None;
        }
        let mut j = i + 1;
        while j < insns.len()
            && insns[j].op == Op::Add
            && !self.is_target[j]
            && self.ins[j].is_some()
        {
            j += 1;
        }
        let n = j - i - 1;
        if n == 0
            || j >= insns.len()
            || insns[j].op != Op::Star
            || self.is_target[j]
            || self.ins[j].is_none()
        {
            return None;
        }
        let (a, c) = (insns[i].reg as usize, insns[j].reg as usize);
        let adds = i + 1..j;
        if adds.clone().any(|k| insns[k].reg as usize == c) {
            return None;
        }
        let reg_ty = |k: usize, r: usize| self.ins[k].as_ref().map(|st| st.regs[r]);
        let mut steps = Vec::new();
        let step = |name: String, insn: usize, undo: Option<(String, i32)>| FusedStep {
            name,
            imm: 0,
            insn,
            undo,
        };
        match self.homes[c] {
            Home::Int(cs) => {
                if a != c {
                    let Home::Int(as_) = self.homes[a] else {
                        return None;
                    };
                    steps.push(step(format!("mov_u{cs}_u{as_}"), i + 1, None));
                }
                for k in adds {
                    let b = insns[k].reg as usize;
                    let st = self.ins[k].as_ref()?;
                    let Home::Int(bs) = self.homes[b] else {
                        return None;
                    };
                    if st.acc != Ty::I32 || st.regs[b] != Ty::I32 || widened.contains(&k) {
                        return None;
                    }
                    let undo = (format!("undo_addr_u{cs}_u{bs}"), 0);
                    steps.push(step(format!("addr_ii_u{cs}_u{bs}"), k, Some(undo)));
                }
            }
            Home::Fpr(cs) => {
                if self.ins[j].as_ref()?.acc != Ty::F64 {
                    return None;
                }
                if a != c {
                    let name = match (self.homes[a], reg_ty(i, a)?) {
                        (Home::Fpr(as_), Ty::F64) => format!("mov_f{cs}_f{as_}"),
                        (Home::Int(as_), Ty::I32) => format!("mov_f{cs}_u{as_}"),
                        _ => return None,
                    };
                    steps.push(step(name, i + 1, None));
                }
                for k in adds {
                    let b = insns[k].reg as usize;
                    let name = match (self.homes[b], reg_ty(k, b)?) {
                        (Home::Fpr(bs), Ty::F64) => format!("addr_ff_f{cs}_f{bs}"),
                        (Home::Int(bs), Ty::I32) => format!("addr_fi_f{cs}_u{bs}"),
                        (Home::Gpr(bs), Ty::I32) => format!("addr_fi_f{cs}_g{bs}"),
                        _ => return None,
                    };
                    steps.push(step(name, k, None));
                }
            }
            _ => return None,
        }
        Some(Fused {
            len: n + 2,
            steps,
            ldar: format!("ldar_{}", self.homes[c].loc()),
            acc_live_after: self.live_out[j],
        })
    }
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
    /// entry, or covered by the fused stencil of an earlier instruction),
    /// for inspection and tests.
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
    fusion: bool,
    unboxed_ints: bool,
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

        // Registers not live at entry are `Bot` there: their entry values
        // (typically `undefined`) are never read, so they shouldn't widen the
        // types the region's own stores give them.
        let reach = reachable(insns, entry);
        let live_in = reg_live_in(insns, &reach, nregs);
        let live_at_entry = live_in[entry].clone();
        let mut actual_live = actual.clone();
        for (r, &live) in live_at_entry.iter().enumerate() {
            if !live {
                actual_live.regs[r] = Ty::Bot;
            }
        }

        // Pass 1: types without pinning; choose homes. Pass 2: types with
        // the FP-homed registers pinned as doubles. Pinning only widens
        // int32 to double, so it can't make an FP-pinned register `Any`, but
        // it can turn an int-pinned one double; then unpin it and retry.
        let unpinned = vec![false; nregs];
        let spec1 = Spec {
            widened,
            pinned: &unpinned,
        };
        let ins1 = infer(insns, entry, actual_live.clone(), &spec1);
        let no_widening = HashSet::new();
        let pin_ok = pinnable(
            insns,
            entry,
            &reach,
            &live_at_entry,
            &ins1,
            widened.unwrap_or(&no_widening),
        );
        let (mut no_fp, mut no_int) = (vec![false; nregs], vec![!opts.unboxed_ints; nregs]);
        let (homes, ins) = loop {
            let homes = if opts.register_homes {
                assign_homes(insns, &ins1, nregs, &no_fp, &no_int, &pin_ok)
            } else {
                vec![Home::Mem; nregs]
            };
            let pinned: Vec<bool> = homes.iter().map(|h| matches!(h, Home::Fpr(_))).collect();
            let mut entry_state = actual_live.clone();
            for (r, &p) in pinned.iter().enumerate() {
                if p && live_at_entry[r] {
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
            let strays = |r: usize, ok: Ty| {
                ins.iter()
                    .flatten()
                    .any(|st| st.regs[r] != ok && st.regs[r] != Ty::Bot)
            };
            let mut retry = false;
            for (r, h) in homes.iter().enumerate() {
                match h {
                    Home::Fpr(_) if strays(r, Ty::F64) => (no_fp[r], retry) = (true, true),
                    Home::Int(_) if strays(r, Ty::I32) => (no_int[r], retry) = (true, true),
                    _ => {}
                }
            }
            if !retry {
                break (homes, ins);
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

        let entry_seq = entry_pieces(
            &actual,
            &actual_live,
            ins[entry].as_ref().unwrap(),
            &homes,
            &live_at_entry,
        );
        let has_entry_seq = !entry_seq.is_empty();
        hot.extend(ending_at(entry_seq, Target::Label(entry)));

        let live_out = acc_live_out(insns, &ins);
        let mut is_target = vec![false; n];
        is_target[entry] = true;
        for (insn, st) in insns.iter().zip(&ins) {
            if st.is_some() && insn.jumps() {
                is_target[insn.target] = true;
            }
        }
        let fusion = Fusion {
            insns,
            ins: &ins,
            homes: &homes,
            widened: widened.filter(|_| opts.fusion),
            live_out: &live_out,
            is_target: &is_target,
        };

        // The jump target of instruction `i` (with its out-state), going
        // through a conversion stub if the edge needs one.
        let jump_to = |i: usize, out: &TypeState, stubs: &mut Vec<Vec<Piece>>| {
            let target = insns[i].target;
            let cs = edge_conversions(out, ins[target].as_ref().unwrap(), &homes);
            if cs.is_empty() {
                Target::Label(target)
            } else {
                stubs.push(ending_at(cs, Target::Label(target)));
                Target::Stub(stubs.len() - 1)
            }
        };

        let mut i = 0;
        while i < n {
            let (Some(s), Some(st)) = (&sel[i], &ins[i]) else {
                i += 1;
                continue;
            };
            let insn = &insns[i];
            if i > 0
                && let Some(prev) = &sel[i - 1]
                && insns[i - 1].falls_through()
            {
                hot.extend(edge_conversions(&prev.out, st, &homes));
            }
            label_piece[i] = hot.len();

            if let Some(f) = fusion.at(i) {
                let last = i + f.len - 1;
                let jump = if insns[last].jumps() {
                    jump_to(last, &sel[last].as_ref().unwrap().out, &mut stubs)
                } else {
                    Target::Next
                };
                let n_steps = f.steps.len();
                for (k, step) in f.steps.iter().enumerate() {
                    let piece = Piece {
                        jump: if k + 1 == n_steps { jump } else { Target::Next },
                        exit: step.insn,
                        ..Piece::with_imm(&step.name, step.imm)
                    };
                    if piece.uses(Sym::Exit) {
                        // Exit before the step's instruction: undo the
                        // in-place op, then `acc` is the result slot.
                        let mut seq = Vec::new();
                        if let Some((undo, imm)) = &step.undo {
                            seq.push(Piece {
                                imm: *imm,
                                ..Piece::new(undo, 0)
                            });
                        }
                        seq.push(Piece::new(&f.ldar, 0));
                        seq.push(Piece::new("box_iacc", 0));
                        seq.extend(spills(&homes));
                        seq.push(Piece {
                            imm: insns[step.insn].pc as i32,
                            ..Piece::new("exit", 0)
                        });
                        exits.push((step.insn, seq));
                    }
                    hot.push(piece);
                }
                if f.acc_live_after {
                    hot.push(Piece::new(&f.ldar, 0));
                }
                names[i] = Some(
                    f.steps
                        .iter()
                        .map(|s| s.name.as_str())
                        .collect::<Vec<_>>()
                        .join("+"),
                );
                i += f.len;
                continue;
            }

            let (pre, name) = stencil_for(insn, s, st, &homes);
            let jump = if insn.jumps() {
                jump_to(i, &s.out, &mut stubs)
            } else {
                Target::Next
            };
            hot.extend(pre);
            let main = Piece {
                reg: insn.reg,
                jump,
                exit: i,
                ..Piece::with_imm(&name, insn.imm)
            };
            if main.uses(Sym::Exit) {
                assert_ne!(st.acc, Ty::F64, "exits happen at int32 operations");
                // `add_smi_i` adds in place; undo it before anything else.
                let mut seq: Vec<Piece> = Vec::new();
                if name == "add_smi_i" {
                    seq.push(Piece {
                        imm: insn.imm,
                        ..Piece::new("undo_add_smi_i", 0)
                    });
                }
                seq.extend(box_acc_piece(st.acc));
                seq.extend(spills(&homes));
                seq.push(Piece {
                    imm: insn.pc as i32,
                    ..Piece::new("exit", 0)
                });
                exits.push((i, seq));
            }
            hot.push(main);
            names[i] = Some(name);
            i += 1;
        }

        // Positions. Hot pieces back to back; then cold code: edge stubs,
        // exit stubs, helpers; then runtime-call wrappers.
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
        // `__slow` helpers, per hot piece: piece index -> offset.
        let mut helper_at: HashMap<usize, usize> = HashMap::new();
        let mut helper_list = Vec::new();
        for (k, p) in hot.iter().enumerate() {
            if let Some(h) = p.slow {
                helper_at.insert(k, pos);
                helper_list.push((k, h, pos));
                pos += h.stencil.code.len();
            }
        }
        // Runtime-call wrappers, saving the slot registers this region uses
        // (see `rt_call!` in the stencils). The FP accumulator counts if the
        // region ever holds a double in it.
        let uses_facc = ins.iter().flatten().any(|st| st.acc == Ty::F64)
            || sel.iter().flatten().any(|s| s.out.acc == Ty::F64);
        let saved_gprs: Vec<u8> = homes
            .iter()
            .filter_map(|h| match h {
                Home::Gpr(k) | Home::Int(k) => arch::caller_saved_gpr_slot(*k),
                _ => None,
            })
            .collect();
        let saved_fprs: Vec<u8> = uses_facc
            .then_some(0)
            .into_iter()
            .chain(homes.iter().filter_map(|h| match h {
                Home::Fpr(k) => Some(k + 1),
                _ => None,
            }))
            .collect();
        pos = pos.next_multiple_of(arch::CODE_ALIGN);
        let mut tramp_at: HashMap<u8, usize> = HashMap::new();
        let mut wrappers: Vec<(usize, Vec<u8>)> = Vec::new();
        let all_stencils = hot
            .iter()
            .chain(stubs.iter().flatten())
            .chain(exits.iter().flat_map(|(_, s)| s))
            .map(|p| p.stencil)
            .chain(helper_list.iter().map(|&(_, h, _)| h));
        for s in all_stencils {
            for r in s.stencil.relocs {
                let Sym::Rt(rt) = r.2 else { continue };
                tramp_at.entry(rt).or_insert_with(|| {
                    let target = runtime_function(RT_NAMES[rt as usize]) as u64;
                    let code = arch::runtime_wrapper(target, &saved_gprs, &saved_fprs);
                    let at = pos;
                    pos += code.len().next_multiple_of(8);
                    wrappers.push((at, code));
                    at
                });
            }
        }

        let mut mem = ExecMem::new(pos);
        let base = mem.ptr as u64;
        let buf = mem.bytes();
        for (at, code) in &wrappers {
            buf[*at..*at + code.len()].copy_from_slice(code);
        }

        let resolve = |t: Target, next: usize| match t {
            Target::Next => next,
            Target::Label(i) => label(i),
            Target::Stub(s) => stub_ats[s][0],
        };
        // Copy the first `len` bytes of `s` to `at` and patch it: register
        // fields of a family template, operands from `p`, continuation and
        // branch target at the given offsets, and the helper of hot piece
        // `key`.
        let mut emit = |s: Instance,
                        len: usize,
                        at: usize,
                        p: &Piece,
                        cont: usize,
                        jump: usize,
                        key: Option<usize>| {
            s.copy_to(&mut buf[at..], len);
            for r in s.stencil.relocs.iter().filter(|r| (r.0 as usize) < len) {
                let value = match r.2 {
                    Sym::HoleImm => p.imm as u32 as u64,
                    Sym::HoleReg => p.reg as u64 * size_of::<Value>() as u64,
                    Sym::Continue => base + cont as u64,
                    Sym::JumpTarget => base + jump as u64,
                    Sym::Exit => base + exit_at[&p.exit] as u64,
                    Sym::Rt(rt) => base + tramp_at[&rt] as u64,
                    Sym::Slow => {
                        let k = key.expect("helper reference outside a hot piece");
                        base + helper_at[&k] as u64
                    }
                };
                let field = at + r.0 as usize;
                arch::patch(
                    &mut buf[field..],
                    r.1 as u32,
                    value,
                    r.3 as i64,
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
            emit(h, h.stencil.code.len(), at, &hot[k], cont, jump, Some(k));
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
                fusion: true,
                unboxed_ints: true,
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
            fusion: false,
            unboxed_ints: false,
        };
        self
    }

    /// Don't use fused stencils.
    pub fn without_fusion(mut self) -> JitFunction {
        self.opts.fusion = false;
        self.opts.unboxed_ints = false;
        self
    }

    /// Keep int32 values boxed in slots.
    pub fn without_unboxed_ints(mut self) -> JitFunction {
        self.opts.unboxed_ints = false;
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
                    let k = self.index_of(exit_pc);
                    self.widened.insert(k);
                    self.cached = None;
                    (pc, acc) = (exit_pc, exit_acc);
                    // Resume at a preceding `ldar r` if `acc` is just `r`.
                    // Re-running it changes nothing (acc is dead before an
                    // `ldar`), and the resume point then starts a fusible
                    // group (`ldar r; add …; star r`) instead of landing
                    // inside one, where nothing could be fused.
                    if k > 0
                        && let prev = &self.insns[k - 1]
                        && prev.op == Op::Ldar
                        && regs[prev.reg as usize] == exit_acc
                    {
                        pc = prev.pc;
                    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn fnv1a(bytes: &[u8]) -> u64 {
        bytes.iter().fold(0xcbf2_9ce4_8422_2325, |h, &b| {
            (h ^ b as u64).wrapping_mul(0x100_0000_01b3)
        })
    }

    /// Every stencil variant LLVM compiled is reproduced exactly by the JIT's
    /// lookup and copy, including (on arm64) those rebuilt from a family
    /// template by patching register fields.
    #[test]
    fn every_stencil_instance_matches_its_compiled_bytes() {
        for &(name, hash) in STENCIL_HASHES {
            let inst = Instance::get(name);
            let len = inst.stencil.code.len();
            let mut buf = vec![0; len];
            inst.copy_to(&mut buf, len);
            assert_eq!(fnv1a(&buf), hash, "{name} (from {})", inst.stencil.name);
        }
    }
}
