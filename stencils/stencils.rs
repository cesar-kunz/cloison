//! Copy-and-patch stencils for the baseline JIT (`src/jit.rs`).
//!
//! `build.rs` compiles this file on its own into an object file, extracts
//! each `toi_stencil_*` function's machine code and relocations, and
//! generates tables for the JIT. The JIT copies one stencil per bytecode
//! instruction and patches the relocations.
//!
//! Stencils are written like the interpreter handlers
//! (`extern "rust-preserve-none"`, ending in `become`), with three
//! differences:
//! - There is no `pc` and no dispatch. The next instruction is the `extern`
//!   function `toi_continue`, a relocation the JIT patches to point at the
//!   next stencil. When that stencil comes right after, the JIT drops the
//!   trailing `jmp`. Branches go to `toi_jump_target` the same way.
//! - Operands are holes: `extern` statics whose *address* is the operand
//!   (`toi_hole_imm`, `toi_hole_reg`). The JIT patches each address
//!   relocation with the operand's value. Stencils must only use the low
//!   32 bits of `toi_hole_imm`: on x86-64 the small code model may emit it
//!   sign- or zero-extended.
//! - Slow paths that call into the runtime (`toi_rt_*`, C ABI) live in
//!   separate `#[cold]` helper functions named `toi_stencil_<op>__<name>`.
//!   Otherwise LLVM sets up a stack frame on the fast path too. The JIT
//!   copies each instruction's helpers into a cold area, and `toi_rt_*`
//!   calls go through trampolines in JIT memory.
//!
//! Signature: `(acc, regs, tag)`, in r12–r14 (x86-64) or x20–x22 (arm64).
//! State is the interpreter's `ANY` state: `acc` is always boxed.

#![feature(explicit_tail_calls, rust_preserve_none_cc)]
#![allow(incomplete_features, non_snake_case, unused_unsafe, clippy::missing_safety_doc)]

#[path = "../src/value.rs"]
#[allow(dead_code)]
mod value;

use value::{NumberTag, Value};

unsafe extern "C" {
    static toi_hole_imm: u8;
    static toi_hole_reg: u8;

    fn toi_rt_add(a: Value, b: Value) -> Value;
    fn toi_rt_inc(a: Value) -> Value;
    fn toi_rt_less_than(a: Value, b: Value) -> bool;
}

unsafe extern "rust-preserve-none" {
    fn toi_continue(acc: Value, regs: *mut Value, tag: NumberTag) -> Value;
    fn toi_jump_target(acc: Value, regs: *mut Value, tag: NumberTag) -> Value;
}

/// The `i32` immediate operand.
#[inline(always)]
fn imm() -> i32 {
    unsafe { &raw const toi_hole_imm as usize as u32 as i32 }
}

/// The immediate's low 32 bits, zero-extended.
#[cfg(target_arch = "x86_64")]
#[inline(always)]
fn imm_zext() -> u64 {
    // A single `mov eax, imm32`.
    imm() as u32 as u64
}

/// The immediate's low 32 bits, zero-extended.
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn imm_zext() -> u64 {
    // `adrp` + `add` yield the patched value exactly (it is < 2^32), so
    // skip the truncation, which would cost a `mov w8, w8`.
    unsafe { &raw const toi_hole_imm as usize as u64 }
}

/// The register operand: `regs` plus a patched byte offset.
#[inline(always)]
unsafe fn reg(regs: *mut Value) -> *mut Value {
    unsafe { regs.byte_add(&raw const toi_hole_reg as usize) }
}

macro_rules! stencils {
    ($( $(#[$m:meta])* fn $name:ident($acc:ident, $regs:ident, $tag:ident) $body:block )*) => {$(
        $(#[$m])*
        #[unsafe(no_mangle)]
        pub unsafe extern "rust-preserve-none" fn $name(
            $acc: Value,
            $regs: *mut Value,
            $tag: NumberTag,
        ) -> Value {
            unsafe { $body }
        }
    )*};
}

stencils! {
    fn toi_stencil_nop(acc, regs, tag) {
        become toi_continue(acc, regs, tag)
    }

    fn toi_stencil_lda_smi(acc, regs, tag) {
        let _ = acc;
        become toi_continue(Value::from_bits(tag.to_bits() | imm_zext()), regs, tag)
    }

    fn toi_stencil_ldar(acc, regs, tag) {
        let _ = acc;
        become toi_continue(*reg(regs), regs, tag)
    }

    fn toi_stencil_star(acc, regs, tag) {
        *reg(regs) = acc;
        become toi_continue(acc, regs, tag)
    }

    fn toi_stencil_add(acc, regs, tag) {
        let rhs = *reg(regs);
        if tag.both_int32(acc, rhs)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(rhs.as_i32_unchecked())
        {
            become toi_continue(tag.box_i32(sum), regs, tag)
        }
        become toi_stencil_add__slow(acc, regs, tag)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_add__slow(acc, regs, tag) {
        become toi_continue(toi_rt_add(acc, *reg(regs)), regs, tag)
    }

    fn toi_stencil_add_smi(acc, regs, tag) {
        if tag.is_int32(acc)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(imm())
        {
            become toi_continue(tag.box_i32(sum), regs, tag)
        }
        become toi_stencil_add_smi__slow(acc, regs, tag)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_add_smi__slow(acc, regs, tag) {
        become toi_continue(toi_rt_add(acc, Value::from_i32(imm())), regs, tag)
    }

    fn toi_stencil_inc(acc, regs, tag) {
        if tag.is_int32(acc)
            && let Some(n) = acc.as_i32_unchecked().checked_add(1)
        {
            become toi_continue(tag.box_i32(n), regs, tag)
        }
        become toi_stencil_inc__slow(acc, regs, tag)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_inc__slow(acc, regs, tag) {
        become toi_continue(toi_rt_inc(acc), regs, tag)
    }

    fn toi_stencil_jump_if_less(acc, regs, tag) {
        let rhs = *reg(regs);
        if !tag.both_int32(acc, rhs) {
            become toi_stencil_jump_if_less__slow(acc, regs, tag)
        }
        if acc.as_i32_unchecked() < rhs.as_i32_unchecked() {
            become toi_jump_target(acc, regs, tag)
        }
        become toi_continue(acc, regs, tag)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_jump_if_less__slow(acc, regs, tag) {
        if toi_rt_less_than(acc, *reg(regs)) {
            become toi_jump_target(acc, regs, tag)
        }
        become toi_continue(acc, regs, tag)
    }

    fn toi_stencil_jump_if_less_smi(acc, regs, tag) {
        if !tag.is_int32(acc) {
            become toi_stencil_jump_if_less_smi__slow(acc, regs, tag)
        }
        if acc.as_i32_unchecked() < imm() {
            become toi_jump_target(acc, regs, tag)
        }
        become toi_continue(acc, regs, tag)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_jump_if_less_smi__slow(acc, regs, tag) {
        if toi_rt_less_than(acc, Value::from_i32(imm())) {
            become toi_jump_target(acc, regs, tag)
        }
        become toi_continue(acc, regs, tag)
    }

    fn toi_stencil_jump(acc, regs, tag) {
        become toi_jump_target(acc, regs, tag)
    }

    fn toi_stencil_return(acc, regs, tag) {
        let _ = (regs, tag);
        acc
    }
}
