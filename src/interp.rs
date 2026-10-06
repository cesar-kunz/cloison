//! The tail-calling dispatcher.
//!
//! Interpreter state lives entirely in arguments, so with `preserve_none` it
//! lives entirely in registers:
//!
//! | arg    | x86-64 | arm64 | meaning                                  |
//! |--------|--------|-------|------------------------------------------|
//! | `pc`   | r12    | x20   | pointer to the current instruction       |
//! | `acc`  | r13    | x21   | the accumulator                          |
//! | `regs` | r14    | x22   | pointer to the register file             |
//! | `tag`  | r15    | x23   | [`NumberTag`], pinned like JSC's `numberTag` register |
//! | `tbl`  | rdi    | x24   | the handler table (`ANY` state)          |
//! | `ti32` | rsi    | x25   | the handler table for the `I32` accumulator state |
//!
//! Two x86-64 constraints shape this list. Both decide whether LLVM can
//! fold the table load into the tail jump (`jmp [tbl + op*8]`). If it
//! can't, every dispatch pays an extra `mov`:
//!
//! - The jump's address registers must be in `GR64_TC` (caller-saved under
//!   the C ABI), which excludes r12–r15. So both tables go in args 5–6
//!   (rdi, rsi).
//! - At most 6 arguments may be in registers (the `X86tcret_6regs` predicate
//!   on `TCRETURNmi64`). So the argument list is full. VM-wide state (heap,
//!   constants, …) will live in a frame header next to `regs`, as in JSC and
//!   V8, not in a 7th argument.
//!
//! On arm64 neither applies: the table only feeds an `ldr`.
//!
//! Dispatch to the next instruction is: load opcode byte, advance `pc`,
//! indirect jump through `tbl`.
//!
//! Value handlers are tiered like JSC's LLInt:
//! 1. An inline int32 fast path in the handler.
//! 2. A `become` to a cold `slow_*` handler with the same signature, for
//!    any mix of int32 and double. It boxes the result as a double without
//!    normalizing integral results back to int32.
//! 3. A plain call to a `generic_*` runtime function for anything else.
//!    That is the only tier that sets up a stack frame.
//!
//! # Accumulator type state (feature `acc-i32`, on by default)
//!
//! What is known about `acc`'s type is encoded in which table we dispatch
//! through, like Ertl's stack caching but caching a type:
//!
//! - `ANY` (`tbl`): `acc` is a boxed [`Value`] of unknown type.
//! - `I32` (`ti32`): `acc` is known to be an int32. Its low 32 bits hold the
//!   value. Its high bits are 0 (a fresh arithmetic result, left unboxed) or
//!   `NUMBER_TAG` (a boxed int that was just checked). Either way, reboxing
//!   is a single `or` with the tag.
//!
//! Handlers that produce an int32 (`add`, `inc`, `lda_smi`, …) or that have
//! just checked one (`jump_if_less`) continue in `I32`. There, `I32`
//! variants skip the accumulator's type check and the reboxing of
//! intermediate results; `acc` is boxed only when stored (`star`) or when
//! leaving the state. Opcodes without an `I32` variant go through
//! [`i32_to_any`], which reboxes and re-dispatches through `ANY`. Both tables
//! sit in their own registers, so neither staying nor switching costs
//! anything.

use crate::bytecode::Op;
use crate::value::{NumberTag, Value};

pub type Handler = extern "rust-preserve-none" fn(
    *const u8,
    Value,
    *mut Value,
    NumberTag,
    &'static Table,
    &'static Table,
) -> Value;

/// Whether int32 results continue in the `I32` accumulator state. Without
/// it, they're boxed and every handler runs in `ANY`, as before. That
/// keeps the effect of the state tables measurable.
const ACC_I32: bool = cfg!(feature = "acc-i32");

#[repr(transparent)]
pub struct Table([Handler; 256]);

/// The int32 in an `I32`-state accumulator.
#[inline(always)]
fn raw_i32(acc: Value) -> i32 {
    acc.to_bits() as u32 as i32
}

/// An int32 as an `I32`-state accumulator, left unboxed.
#[inline(always)]
fn unboxed(i: i32) -> Value {
    Value::from_bits(i as u32 as u64)
}

/// Tail-call the handler of the instruction at `$pc + $off` (bytes, signed)
/// through `$via`, one of the two tables.
macro_rules! dispatch {
    ($via:expr, $pc:expr, $off:expr, $acc:expr, $regs:expr, $tag:expr, $tbl:expr, $ti32:expr) => {{
        // Evaluate operands before the barrier, while `pc` is still current.
        let acc: Value = $acc;
        let next: *const u8 = $pc.offset($off as isize);
        pc_barrier!(next);
        let op = *next;
        become ($via.0[op as usize])(next, acc, $regs, $tag, $tbl, $ti32)
    }};
}

/// An empty `asm!` placed just before dispatch. It emits no code; its job is
/// to steer the register allocator. Not being `pure`, it can't be reordered,
/// so a handler's operand reads stay before the pc update. `nomem` keeps it
/// from acting as a compiler memory fence. Without it, the old and new `pc`
/// end up live together and LLVM copies pc around: `lea rax,[r12+N] … mov
/// r12,rax` on x86, `add x8,x20,#N … mov x20,x8` on arm64.
///
/// x86-64: also launder `next` through a register. Otherwise LLVM folds
/// `pc + off` into the opcode load (`movzx ecx,[r12+rcx]`), which keeps the
/// old pc alive again. Result: `add r12,N; movzx eax,[r12]; jmp [rdi+rax*8]`.
#[cfg(target_arch = "x86_64")]
macro_rules! pc_barrier {
    ($next:ident) => {
        let mut $next = $next;
        core::arch::asm!("/* {0} */", inout(reg) $next, options(nomem, nostack, preserves_flags));
    };
}

/// AArch64: no laundering. LLVM must see `pc + N` feeding the load so it can
/// fuse them into a pre-indexed `ldrb w8,[x20,#N]!`. Laundering would split
/// it back into `add` + `ldrb`. Result: `ldrb w8,[x20,#N]!; ldr x0,[x24,x8,lsl
/// #3]; br x0`. There is no memory-indirect branch, so 3 is the floor.
#[cfg(target_arch = "aarch64")]
macro_rules! pc_barrier {
    ($next:ident) => {
        core::arch::asm!("", options(nomem, nostack, preserves_flags));
    };
}

/// One back edge: count down the frame's budget and, when it runs out,
/// leave to tier up at `$target` with `$acc` (boxed).
///
/// x86-64: `sub qword ptr [regs - 8], 1; jle osr`, through `asm goto`. LLVM
/// otherwise loads, decrements, stores and tests (5 instructions).
#[cfg(target_arch = "x86_64")]
macro_rules! back_edge {
    ($regs:ident, $target:expr, $acc:expr) => {
        core::arch::asm!(
            "sub qword ptr [{r} - {off}], 1",
            "jle {osr}",
            r = in(reg) $regs,
            off = const BUDGET * size_of::<Value>(),
            osr = label { return unsafe { osr_exit($regs, $target, $acc) }; },
            options(nostack),
        )
    };
}

/// arm64 has no memory-destination arithmetic: `ldr; subs; str; b.le`.
#[cfg(target_arch = "aarch64")]
macro_rules! back_edge {
    ($regs:ident, $target:expr, $acc:expr) => {
        let budget = $regs.cast::<i64>().sub(BUDGET);
        *budget -= 1;
        if *budget <= 0 {
            return osr_exit($regs, $target, $acc);
        }
    };
}

/// Read an unaligned little-endian `i32` operand at `pc + off`.
#[inline(always)]
unsafe fn imm32(pc: *const u8, off: usize) -> i32 {
    unsafe { i32::from_le(pc.add(off).cast::<i32>().read_unaligned()) }
}

/// Read a register operand at `pc + off` and return a pointer to that register.
#[inline(always)]
unsafe fn reg(pc: *const u8, off: usize, regs: *mut Value) -> *mut Value {
    unsafe { regs.add(*pc.add(off) as usize) }
}

/// Both operands as `f64` if both are numbers (int32 or double).
#[inline(always)]
fn both_numbers(a: Value, b: Value) -> Option<(f64, f64)> {
    Some((a.as_number()?, b.as_number()?))
}

/// Defines handlers with the uniform signature. A body names `pc`, `acc`,
/// `regs` and `tag`, and leaves through the local macros:
///
/// - `next!(any | i32, off, acc)` dispatches to `pc + off` in that state.
/// - `next_int!(off, i)` continues with the int32 `i` as `acc`: unboxed in
///   `I32` with `acc-i32`, otherwise boxed in `ANY`.
/// - `tail!(f, acc)` tail-calls handler `f` on the same instruction.
///
/// (The `$`-variables of the local macros aren't bound by `handlers!`, so
/// they pass through its expansion untouched.)
macro_rules! handlers {
    ($( $(#[$m:meta])* fn $name:ident($pc:ident, $acc:ident, $regs:ident, $tag:ident) $body:block )*) => {$(
        $(#[$m])*
        #[allow(unused_assignments, unused_macros, unused_variables)]
        // `dispatch!` hands `next` to a `nomem` asm block, which never dereferences it.
        #[allow(clippy::pointers_in_nomem_asm_block)]
        extern "rust-preserve-none" fn $name(
            $pc: *const u8,
            #[allow(unused_mut)] mut $acc: Value,
            $regs: *mut Value,
            $tag: NumberTag,
            tbl: &'static Table,
            ti32: &'static Table,
        ) -> Value {
            macro_rules! next {
                (any, $o:expr, $a:expr) => { dispatch!(tbl, $pc, $o, $a, $regs, $tag, tbl, ti32) };
                (i32, $o:expr, $a:expr) => { dispatch!(ti32, $pc, $o, $a, $regs, $tag, tbl, ti32) };
            }
            macro_rules! next_int {
                ($o:expr, $i:expr) => {
                    if ACC_I32 { next!(i32, $o, unboxed($i)) } else { next!(any, $o, $tag.box_i32($i)) }
                };
            }
            macro_rules! tail {
                ($f:expr, $a:expr) => { become $f($pc, $a, $regs, $tag, tbl, ti32) };
            }
            #[allow(unused_unsafe)]
            unsafe { $body }
        }
    )*};
}

handlers! {
    // ---- ANY state: `acc` is a boxed Value of unknown type ----

    fn op_nop(pc, acc, regs, tag) {
        next!(any, Op::Nop.size(), acc)
    }

    // Also the I32 entry: the result is an int32 whatever the state.
    fn op_lda_smi(pc, acc, regs, tag) {
        next_int!(Op::LdaSmi.size(), imm32(pc, 1))
    }

    // Also the I32 entry: the loaded value's type is unknown.
    fn op_ldar(pc, acc, regs, tag) {
        acc = *reg(pc, 1, regs);
        next!(any, Op::Ldar.size(), acc)
    }

    fn op_star(pc, acc, regs, tag) {
        *reg(pc, 1, regs) = acc;
        next!(any, Op::Star.size(), acc)
    }

    fn op_add(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        if tag.both_int32(acc, rhs)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(rhs.as_i32_unchecked())
        {
            next_int!(Op::Add.size(), sum)
        }
        tail!(slow_add, acc)
    }

    fn op_add_smi(pc, acc, regs, tag) {
        if tag.is_int32(acc)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(imm32(pc, 1))
        {
            next_int!(Op::AddSmi.size(), sum)
        }
        tail!(slow_add_smi, acc)
    }

    fn op_inc(pc, acc, regs, tag) {
        if tag.is_int32(acc)
            && let Some(n) = acc.as_i32_unchecked().checked_add(1)
        {
            next_int!(Op::Inc.size(), n)
        }
        tail!(slow_inc, acc)
    }

    fn op_jump_if_less(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        if !tag.both_int32(acc, rhs) {
            tail!(slow_jump_if_less, acc)
        }
        // Two separate dispatches, not one on a selected pc. A select
        // materializes the fall-through length (`mov eax, 6`) and blocks the
        // pre-indexed load on arm64. `acc` was just checked, so with
        // `acc-i32` we continue in I32 (a boxed int meets its invariant).
        let less = acc.as_i32_unchecked() < rhs.as_i32_unchecked();
        if ACC_I32 {
            if less { next!(i32, imm32(pc, 2), acc) } else { next!(i32, Op::JumpIfLess.size(), acc) }
        } else {
            if less { next!(any, imm32(pc, 2), acc) } else { next!(any, Op::JumpIfLess.size(), acc) }
        }
    }

    fn op_jump_if_less_smi(pc, acc, regs, tag) {
        if !tag.is_int32(acc) {
            tail!(slow_jump_if_less_smi, acc)
        }
        let less = acc.as_i32_unchecked() < imm32(pc, 1);
        if ACC_I32 {
            if less { next!(i32, imm32(pc, 5), acc) } else { next!(i32, Op::JumpIfLessSmi.size(), acc) }
        } else {
            if less { next!(any, imm32(pc, 5), acc) } else { next!(any, Op::JumpIfLessSmi.size(), acc) }
        }
    }

    fn op_jump(pc, acc, regs, tag) {
        next!(any, imm32(pc, 1), acc)
    }

    // Loop forms: the taken back edge counts down the frame's budget and,
    // at zero, leaves the interpreter to tier up (see `run_frame`).

    fn op_jump_loop(pc, acc, regs, tag) {
        let off = imm32(pc, 1);
        back_edge!(regs, pc.offset(off as isize), acc);
        next!(any, off, acc)
    }

    fn op_jump_if_less_loop(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        if !tag.both_int32(acc, rhs) {
            tail!(slow_jump_if_less_loop, acc)
        }
        if acc.as_i32_unchecked() < rhs.as_i32_unchecked() {
            let off = imm32(pc, 2);
            back_edge!(regs, pc.offset(off as isize), acc);
            if ACC_I32 { next!(i32, off, acc) } else { next!(any, off, acc) }
        }
        if ACC_I32 { next!(i32, Op::JumpIfLess.size(), acc) } else { next!(any, Op::JumpIfLess.size(), acc) }
    }

    fn op_jump_if_less_smi_loop(pc, acc, regs, tag) {
        if !tag.is_int32(acc) {
            tail!(slow_jump_if_less_smi_loop, acc)
        }
        if acc.as_i32_unchecked() < imm32(pc, 1) {
            let off = imm32(pc, 5);
            back_edge!(regs, pc.offset(off as isize), acc);
            if ACC_I32 { next!(i32, off, acc) } else { next!(any, off, acc) }
        }
        if ACC_I32 { next!(i32, Op::JumpIfLessSmi.size(), acc) } else { next!(any, Op::JumpIfLessSmi.size(), acc) }
    }

    fn op_return(pc, acc, regs, tag) {
        let _ = (pc, regs, tag);
        acc
    }

    #[cold]
    fn op_invalid(pc, acc, regs, tag) {
        let _ = (pc, acc, regs, tag);
        std::process::abort()
    }

    // ---- I32 state: `acc` is an int32 (see the module docs) ----

    /// Fallback for opcodes without an I32 variant: box `acc` and handle the
    /// same instruction in ANY.
    fn i32_to_any(pc, acc, regs, tag) {
        next!(any, 0, tag.rebox(acc))
    }

    fn i32_nop(pc, acc, regs, tag) {
        next!(i32, Op::Nop.size(), acc)
    }

    // Rebox `acc` in place (`or r13, r15`) and stay in I32. A boxed int
    // still meets the I32 invariant. Boxing into a scratch register instead
    // costs an extra `mov` on x86.
    fn i32_star(pc, acc, regs, tag) {
        acc = tag.rebox(acc);
        *reg(pc, 1, regs) = acc;
        next!(i32, Op::Star.size(), acc)
    }

    // On overflow, these recover the original `acc` as `wrapped - operand`
    // (cold) instead of keeping it live, so the add can happen in place
    // (`add r13d, eax; jo`), as JSC does.

    fn i32_add(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        if !tag.is_int32(rhs) {
            tail!(slow_add, tag.rebox(acc))
        }
        let (sum, overflow) = raw_i32(acc).overflowing_add(rhs.as_i32_unchecked());
        if overflow {
            let acc = unboxed(sum.wrapping_sub(rhs.as_i32_unchecked()));
            tail!(slow_add, tag.rebox(acc))
        }
        next!(i32, Op::Add.size(), unboxed(sum))
    }

    fn i32_add_smi(pc, acc, regs, tag) {
        let imm = imm32(pc, 1);
        let (sum, overflow) = raw_i32(acc).overflowing_add(imm);
        if overflow {
            tail!(slow_add_smi, tag.rebox(unboxed(sum.wrapping_sub(imm))))
        }
        next!(i32, Op::AddSmi.size(), unboxed(sum))
    }

    fn i32_inc(pc, acc, regs, tag) {
        let (n, overflow) = raw_i32(acc).overflowing_add(1);
        if overflow {
            tail!(slow_inc, tag.rebox(unboxed(n.wrapping_sub(1))))
        }
        next!(i32, Op::Inc.size(), unboxed(n))
    }

    fn i32_jump_if_less(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        if !tag.is_int32(rhs) {
            tail!(slow_jump_if_less, tag.rebox(acc))
        }
        if raw_i32(acc) < rhs.as_i32_unchecked() {
            next!(i32, imm32(pc, 2), acc)
        } else {
            next!(i32, Op::JumpIfLess.size(), acc)
        }
    }

    fn i32_jump_if_less_smi(pc, acc, regs, tag) {
        if raw_i32(acc) < imm32(pc, 1) {
            next!(i32, imm32(pc, 5), acc)
        } else {
            next!(i32, Op::JumpIfLessSmi.size(), acc)
        }
    }

    fn i32_jump(pc, acc, regs, tag) {
        next!(i32, imm32(pc, 1), acc)
    }

    fn i32_jump_loop(pc, acc, regs, tag) {
        let off = imm32(pc, 1);
        back_edge!(regs, pc.offset(off as isize), tag.rebox(acc));
        next!(i32, off, acc)
    }

    fn i32_jump_if_less_loop(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        if !tag.is_int32(rhs) {
            tail!(slow_jump_if_less_loop, tag.rebox(acc))
        }
        if raw_i32(acc) < rhs.as_i32_unchecked() {
            let off = imm32(pc, 2);
            back_edge!(regs, pc.offset(off as isize), tag.rebox(acc));
            next!(i32, off, acc)
        }
        next!(i32, Op::JumpIfLess.size(), acc)
    }

    fn i32_jump_if_less_smi_loop(pc, acc, regs, tag) {
        if raw_i32(acc) < imm32(pc, 1) {
            let off = imm32(pc, 5);
            back_edge!(regs, pc.offset(off as isize), tag.rebox(acc));
            next!(i32, off, acc)
        }
        next!(i32, Op::JumpIfLessSmi.size(), acc)
    }

    fn i32_return(pc, acc, regs, tag) {
        let _ = (pc, regs);
        tag.rebox(acc)
    }

    // ---- Slow paths (tier 2), ANY state ----
    // Same signature as the handlers so they can be `become`d. `inline(never)`
    // keeps LLVM from merging them back in.

    #[cold]
    #[inline(never)]
    fn slow_add(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        acc = match both_numbers(acc, rhs) {
            Some((a, b)) => Value::from_f64_arith(a + b),
            None => generic_add(acc, rhs),
        };
        next!(any, Op::Add.size(), acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_add_smi(pc, acc, regs, tag) {
        let rhs = Value::from_i32(imm32(pc, 1));
        acc = match acc.as_number() {
            Some(a) => Value::from_f64_arith(a + imm32(pc, 1) as f64),
            None => generic_add(acc, rhs),
        };
        next!(any, Op::AddSmi.size(), acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_inc(pc, acc, regs, tag) {
        acc = match acc.as_number() {
            Some(a) => Value::from_f64_arith(a + 1.0),
            None => generic_inc(acc),
        };
        next!(any, Op::Inc.size(), acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_less(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        let less = match both_numbers(acc, rhs) {
            // False if either is NaN.
            Some((a, b)) => a < b,
            None => generic_less_than(acc, rhs),
        };
        if less {
            next!(any, imm32(pc, 2), acc)
        } else {
            next!(any, Op::JumpIfLess.size(), acc)
        }
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_less_smi(pc, acc, regs, tag) {
        let rhs = Value::from_i32(imm32(pc, 1));
        let less = match acc.as_number() {
            Some(a) => a < imm32(pc, 1) as f64,
            None => generic_less_than(acc, rhs),
        };
        if less {
            next!(any, imm32(pc, 5), acc)
        } else {
            next!(any, Op::JumpIfLessSmi.size(), acc)
        }
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_less_loop(pc, acc, regs, tag) {
        let rhs = *reg(pc, 1, regs);
        let less = match both_numbers(acc, rhs) {
            Some((a, b)) => a < b,
            None => generic_less_than(acc, rhs),
        };
        if less {
            let off = imm32(pc, 2);
            back_edge!(regs, pc.offset(off as isize), acc);
            next!(any, off, acc)
        }
        next!(any, Op::JumpIfLess.size(), acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_less_smi_loop(pc, acc, regs, tag) {
        let rhs = Value::from_i32(imm32(pc, 1));
        let less = match acc.as_number() {
            Some(a) => a < imm32(pc, 1) as f64,
            None => generic_less_than(acc, rhs),
        };
        if less {
            let off = imm32(pc, 5);
            back_edge!(regs, pc.offset(off as isize), acc);
            next!(any, off, acc)
        }
        next!(any, Op::JumpIfLessSmi.size(), acc)
    }
}

// Generic runtime (tier 3): ordinary Rust functions, called (not tail-called)
// from the slow paths. Here go ToPrimitive, strings, BigInt and exceptions
// once they exist; they'll reach the VM through the frame. A call from a `preserve_none`
// handler is cheap: the callee preserves r12–r15 / x19–x28, where most of
// the interpreter state already lives.

/// `a + b` where at least one side is not a number.
#[cold]
#[inline(never)]
pub(crate) fn generic_add(a: Value, b: Value) -> Value {
    // ToPrimitive is the identity on everything representable so far, and
    // there are no strings yet, so `+` is numeric addition.
    Value::number(a.to_number() + b.to_number())
}

/// `a + 1` where `a` is not a number.
#[cold]
#[inline(never)]
pub(crate) fn generic_inc(a: Value) -> Value {
    Value::number(a.to_number() + 1.0)
}

/// `a < b` (IsLessThan) where at least one side is not a number.
#[cold]
#[inline(never)]
pub(crate) fn generic_less_than(a: Value, b: Value) -> bool {
    a.to_number() < b.to_number()
}

static TABLE: Table = {
    let mut t: [Handler; 256] = [op_invalid; 256];
    t[Op::Nop as usize] = op_nop;
    t[Op::LdaSmi as usize] = op_lda_smi;
    t[Op::Ldar as usize] = op_ldar;
    t[Op::Star as usize] = op_star;
    t[Op::Add as usize] = op_add;
    t[Op::Inc as usize] = op_inc;
    t[Op::JumpIfLess as usize] = op_jump_if_less;
    t[Op::Jump as usize] = op_jump;
    t[Op::AddSmi as usize] = op_add_smi;
    t[Op::JumpIfLessSmi as usize] = op_jump_if_less_smi;
    t[Op::JumpLoop as usize] = op_jump_loop;
    t[Op::JumpIfLessLoop as usize] = op_jump_if_less_loop;
    t[Op::JumpIfLessSmiLoop as usize] = op_jump_if_less_smi_loop;
    t[Op::Return as usize] = op_return;
    Table(t)
};

static TABLE_I32: Table = {
    let mut t: [Handler; 256] = [i32_to_any; 256];
    t[Op::Nop as usize] = i32_nop;
    t[Op::LdaSmi as usize] = op_lda_smi;
    t[Op::Ldar as usize] = op_ldar;
    t[Op::Star as usize] = i32_star;
    t[Op::Add as usize] = i32_add;
    t[Op::Inc as usize] = i32_inc;
    t[Op::JumpIfLess as usize] = i32_jump_if_less;
    t[Op::Jump as usize] = i32_jump;
    t[Op::AddSmi as usize] = i32_add_smi;
    t[Op::JumpIfLessSmi as usize] = i32_jump_if_less_smi;
    t[Op::JumpLoop as usize] = i32_jump_loop;
    t[Op::JumpIfLessLoop as usize] = i32_jump_if_less_loop;
    t[Op::JumpIfLessSmiLoop as usize] = i32_jump_if_less_smi_loop;
    t[Op::Return as usize] = i32_return;
    Table(t)
};

/// Leave the interpreter to tier up at `target` (a loop header) with `acc`
/// (boxed): record both in the frame header and return the `EMPTY` marker,
/// which no bytecode function returns.
#[cold]
#[inline(never)]
fn osr_exit(regs: *mut Value, target: *const u8, acc: Value) -> Value {
    unsafe {
        *regs.sub(OSR_PC) = Value::from_bits(target as u64);
        *regs.sub(OSR_ACC) = acc;
    }
    Value::EMPTY
}

/// Frame header slots, in front of the register file: `regs[-BUDGET]` is the
/// back-edge budget (an `i64`); `regs[-OSR_PC]` and `regs[-OSR_ACC]` hold the
/// target and accumulator when the interpreter leaves to tier up.
const BUDGET: usize = 1;
const OSR_PC: usize = 2;
const OSR_ACC: usize = 3;
const HEADER: usize = 3;

/// A register file with the interpreter's frame header in front.
pub struct Frame {
    slots: Vec<Value>,
}

impl Frame {
    /// A frame holding a copy of `regs`.
    pub fn new(regs: &[Value]) -> Frame {
        let mut slots = vec![Value::UNDEFINED; HEADER];
        slots.extend_from_slice(regs);
        Frame { slots }
    }

    pub fn regs(&mut self) -> &mut [Value] {
        &mut self.slots[HEADER..]
    }

    fn budget(&self) -> i64 {
        self.slots[HEADER - BUDGET].to_bits() as i64
    }

    fn set_budget(&mut self, n: i64) {
        self.slots[HEADER - BUDGET] = Value::from_bits(n as u64);
    }
}

/// How an interpreter run ended.
pub enum Run {
    Return(Value),
    /// The back-edge budget ran out at the loop header at bytecode offset
    /// `pc`, with `acc`; the register file is up to date.
    Osr {
        pc: usize,
        acc: Value,
    },
}

/// Run `code` from bytecode offset `pc` with accumulator `acc` over `frame`,
/// allowing `budget` loop back edges before leaving to tier up. Returns the
/// remaining budget too.
///
/// # Safety
/// `code` must be well formed: every instruction complete, every register
/// operand `< frame.regs().len()`, every jump landing on an instruction
/// boundary, and every path ending in `Return`. There is no verifier yet.
pub unsafe fn run_frame(
    code: &[u8],
    pc: usize,
    acc: Value,
    frame: &mut Frame,
    budget: i64,
) -> (Run, i64) {
    frame.set_budget(budget.max(1));
    let pc = unsafe { code.as_ptr().add(pc) };
    // Launder the tables (and, in `NumberTag::new`, the tag) so LLVM can't
    // fold them back into constants (`lea`/`movabs`) in every handler. They
    // must stay register arguments.
    let tbl: &'static Table = std::hint::black_box(&TABLE);
    let ti32: &'static Table = std::hint::black_box(&TABLE_I32);
    let tag = NumberTag::new();
    let regs = frame.regs().as_mut_ptr();
    let v = unsafe { (tbl.0[*pc as usize])(pc, acc, regs, tag, tbl, ti32) };
    let left = frame.budget();
    if v != Value::EMPTY {
        return (Run::Return(v), left);
    }
    let target = frame.slots[HEADER - OSR_PC].to_bits() as usize;
    let acc = frame.slots[HEADER - OSR_ACC];
    (
        Run::Osr {
            pc: target - code.as_ptr() as usize,
            acc,
        },
        left,
    )
}

/// Run `code` over the register file `regs` and return the accumulator at
/// `Return`. The accumulator starts as `undefined`.
///
/// # Safety
/// As [`run_frame`], with register operands `< regs.len()`.
pub unsafe fn run(code: &[u8], regs: &mut [Value]) -> Value {
    unsafe { run_from(code, 0, Value::UNDEFINED, regs) }
}

/// Run `code` from bytecode offset `pc` with accumulator `acc`, without
/// tiering up.
///
/// # Safety
/// As [`run`], and `pc` must be an instruction boundary.
pub unsafe fn run_from(code: &[u8], pc: usize, acc: Value, regs: &mut [Value]) -> Value {
    let mut frame = Frame::new(regs);
    let (run, _) = unsafe { run_frame(code, pc, acc, &mut frame, i64::MAX) };
    regs.copy_from_slice(frame.regs());
    match run {
        Run::Return(v) => v,
        Run::Osr { .. } => unreachable!("an unlimited budget never runs out"),
    }
}
