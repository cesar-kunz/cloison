//! Copy-and-patch stencils for the baseline JIT (`src/jit.rs`).
//!
//! `build.rs` compiles this file on its own into an object file, extracts
//! each `toi_stencil_*` function's machine code and relocations, and
//! generates tables for the JIT. The JIT copies one stencil per bytecode
//! instruction (plus conversions on control-flow edges) and patches the
//! relocations.
//!
//! Stencils are written like the interpreter handlers
//! (`extern "rust-preserve-none"`, ending in `become`), with these
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
//! - Side exits leave through `toi_exit`, patched to a per-instruction stub
//!   (the `exit` stencil). It records the instruction's bytecode offset in
//!   `*ctx` and returns `acc` to the JIT driver. An exiting stencil must not
//!   have changed any state yet: the interpreter, or recompiled code,
//!   re-executes the instruction.
//!
//! Signature: `(acc, regs, tag, ctx)`, in r12–r15 (x86-64) or x20–x23
//! (arm64). Values are always boxed in `acc` and in the register file;
//! specialization only removes type checks.
//!
//! Naming: generic stencils (`add`, `inc`, …) handle any types, as the
//! interpreter's `ANY`-state handlers do. Specialized ones carry a suffix
//! with one letter per operand, `i` (int32) or `f` (double), acc first. An
//! extra `_f` means an int32 operation whose result is produced as a double,
//! for instructions that overflowed before (`add_ii_f`).

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
    fn toi_continue(acc: Value, regs: *mut Value, tag: NumberTag, ctx: *mut u32) -> Value;
    fn toi_jump_target(acc: Value, regs: *mut Value, tag: NumberTag, ctx: *mut u32) -> Value;
    fn toi_exit(acc: Value, regs: *mut Value, tag: NumberTag, ctx: *mut u32) -> Value;
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
    ($( $(#[$m:meta])* fn $name:ident($acc:ident, $regs:ident, $tag:ident, $ctx:ident) $body:block )*) => {$(
        $(#[$m])*
        #[unsafe(no_mangle)]
        #[allow(unused_variables)]
        pub unsafe extern "rust-preserve-none" fn $name(
            $acc: Value,
            $regs: *mut Value,
            $tag: NumberTag,
            $ctx: *mut u32,
        ) -> Value {
            unsafe { $body }
        }
    )*};
}

/// Continue with the next stencil.
macro_rules! next {
    ($acc:expr, $regs:ident, $tag:ident, $ctx:ident) => {
        become toi_continue($acc, $regs, $tag, $ctx)
    };
}

/// Take the branch.
macro_rules! jump {
    ($acc:expr, $regs:ident, $tag:ident, $ctx:ident) => {
        become toi_jump_target($acc, $regs, $tag, $ctx)
    };
}

/// Side-exit to the driver, before this instruction has changed anything.
macro_rules! exit {
    ($acc:expr, $regs:ident, $tag:ident, $ctx:ident) => {
        become toi_exit($acc, $regs, $tag, $ctx)
    };
}

stencils! {
    // ---- Type-agnostic ----

    fn toi_stencil_nop(acc, regs, tag, ctx) {
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_lda_smi(acc, regs, tag, ctx) {
        next!(Value::from_bits(tag.to_bits() | imm_zext()), regs, tag, ctx)
    }

    fn toi_stencil_ldar(acc, regs, tag, ctx) {
        next!(*reg(regs), regs, tag, ctx)
    }

    fn toi_stencil_star(acc, regs, tag, ctx) {
        *reg(regs) = acc;
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump(acc, regs, tag, ctx) {
        jump!(acc, regs, tag, ctx)
    }

    fn toi_stencil_return(acc, regs, tag, ctx) {
        acc
    }

    /// Exit stub: record the bytecode offset (`imm`) and return to the driver.
    fn toi_stencil_exit(acc, regs, tag, ctx) {
        *ctx = imm() as u32;
        acc
    }

    // ---- Generic: any types (milestone 1 / interpreter ANY state) ----

    fn toi_stencil_add(acc, regs, tag, ctx) {
        let rhs = *reg(regs);
        if tag.both_int32(acc, rhs)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(rhs.as_i32_unchecked())
        {
            next!(tag.box_i32(sum), regs, tag, ctx)
        }
        become toi_stencil_add__slow(acc, regs, tag, ctx)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_add__slow(acc, regs, tag, ctx) {
        next!(toi_rt_add(acc, *reg(regs)), regs, tag, ctx)
    }

    fn toi_stencil_add_smi(acc, regs, tag, ctx) {
        if tag.is_int32(acc)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(imm())
        {
            next!(tag.box_i32(sum), regs, tag, ctx)
        }
        become toi_stencil_add_smi__slow(acc, regs, tag, ctx)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_add_smi__slow(acc, regs, tag, ctx) {
        next!(toi_rt_add(acc, Value::from_i32(imm())), regs, tag, ctx)
    }

    fn toi_stencil_inc(acc, regs, tag, ctx) {
        if tag.is_int32(acc)
            && let Some(n) = acc.as_i32_unchecked().checked_add(1)
        {
            next!(tag.box_i32(n), regs, tag, ctx)
        }
        become toi_stencil_inc__slow(acc, regs, tag, ctx)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_inc__slow(acc, regs, tag, ctx) {
        next!(toi_rt_inc(acc), regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less(acc, regs, tag, ctx) {
        let rhs = *reg(regs);
        if !tag.both_int32(acc, rhs) {
            become toi_stencil_jump_if_less__slow(acc, regs, tag, ctx)
        }
        if acc.as_i32_unchecked() < rhs.as_i32_unchecked() {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_jump_if_less__slow(acc, regs, tag, ctx) {
        if toi_rt_less_than(acc, *reg(regs)) {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_smi(acc, regs, tag, ctx) {
        if !tag.is_int32(acc) {
            become toi_stencil_jump_if_less_smi__slow(acc, regs, tag, ctx)
        }
        if acc.as_i32_unchecked() < imm() {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    #[cold]
    #[inline(never)]
    fn toi_stencil_jump_if_less_smi__slow(acc, regs, tag, ctx) {
        if toi_rt_less_than(acc, Value::from_i32(imm())) {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    // ---- Specialized: operand types known, no type checks ----
    // `i` = int32, `f` = double, one letter per operand (acc first).

    fn toi_stencil_add_ii(acc, regs, tag, ctx) {
        let rhs = *reg(regs);
        match acc.as_i32_unchecked().checked_add(rhs.as_i32_unchecked()) {
            Some(sum) => next!(tag.box_i32(sum), regs, tag, ctx),
            None => exit!(acc, regs, tag, ctx),
        }
    }

    /// int32 + int32 that overflowed before: always produce a double.
    fn toi_stencil_add_ii_f(acc, regs, tag, ctx) {
        let sum = acc.as_i32_unchecked() as i64 + (*reg(regs)).as_i32_unchecked() as i64;
        next!(tag.encode_f64(sum as f64), regs, tag, ctx)
    }

    fn toi_stencil_add_if(acc, regs, tag, ctx) {
        let sum = acc.as_i32_unchecked() as f64 + tag.decode_f64(*reg(regs));
        next!(tag.encode_f64(sum), regs, tag, ctx)
    }

    fn toi_stencil_add_fi(acc, regs, tag, ctx) {
        let sum = tag.decode_f64(acc) + (*reg(regs)).as_i32_unchecked() as f64;
        next!(tag.encode_f64(sum), regs, tag, ctx)
    }

    fn toi_stencil_add_ff(acc, regs, tag, ctx) {
        let sum = tag.decode_f64(acc) + tag.decode_f64(*reg(regs));
        next!(tag.encode_f64(sum), regs, tag, ctx)
    }

    fn toi_stencil_add_smi_i(acc, regs, tag, ctx) {
        match acc.as_i32_unchecked().checked_add(imm()) {
            Some(sum) => next!(tag.box_i32(sum), regs, tag, ctx),
            None => exit!(acc, regs, tag, ctx),
        }
    }

    fn toi_stencil_add_smi_i_f(acc, regs, tag, ctx) {
        let sum = acc.as_i32_unchecked() as i64 + imm() as i64;
        next!(tag.encode_f64(sum as f64), regs, tag, ctx)
    }

    fn toi_stencil_add_smi_f(acc, regs, tag, ctx) {
        // The immediate comes from a hole, not a float constant: a constant
        // would be a load from .rodata, which stencils can't reference.
        next!(tag.encode_f64(tag.decode_f64(acc) + imm() as f64), regs, tag, ctx)
    }

    // `inc` variants get `imm` = 1 from the JIT, for the same reason.

    fn toi_stencil_inc_i(acc, regs, tag, ctx) {
        match acc.as_i32_unchecked().checked_add(1) {
            Some(n) => next!(tag.box_i32(n), regs, tag, ctx),
            None => exit!(acc, regs, tag, ctx),
        }
    }

    fn toi_stencil_inc_i_f(acc, regs, tag, ctx) {
        next!(tag.encode_f64((acc.as_i32_unchecked() as i64 + 1) as f64), regs, tag, ctx)
    }

    fn toi_stencil_inc_f(acc, regs, tag, ctx) {
        next!(tag.encode_f64(tag.decode_f64(acc) + imm() as f64), regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_ii(acc, regs, tag, ctx) {
        if acc.as_i32_unchecked() < (*reg(regs)).as_i32_unchecked() {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_if(acc, regs, tag, ctx) {
        if (acc.as_i32_unchecked() as f64) < tag.decode_f64(*reg(regs)) {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_fi(acc, regs, tag, ctx) {
        if tag.decode_f64(acc) < (*reg(regs)).as_i32_unchecked() as f64 {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_ff(acc, regs, tag, ctx) {
        if tag.decode_f64(acc) < tag.decode_f64(*reg(regs)) {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_smi_i(acc, regs, tag, ctx) {
        if acc.as_i32_unchecked() < imm() {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    fn toi_stencil_jump_if_less_smi_f(acc, regs, tag, ctx) {
        if tag.decode_f64(acc) < imm() as f64 {
            jump!(acc, regs, tag, ctx)
        }
        next!(acc, regs, tag, ctx)
    }

    // ---- Edge conversions: int32 → double where a join widened the type ----

    fn toi_stencil_conv_acc_f(acc, regs, tag, ctx) {
        next!(tag.encode_f64(acc.as_i32_unchecked() as f64), regs, tag, ctx)
    }

    fn toi_stencil_conv_reg_f(acc, regs, tag, ctx) {
        let r = reg(regs);
        *r = tag.encode_f64((*r).as_i32_unchecked() as f64);
        next!(acc, regs, tag, ctx)
    }
}
