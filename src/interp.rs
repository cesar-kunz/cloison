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

//!
//! # Frames and calls
//!
//! A frame is a register file with a header in front, on the VM's stack:
//!
//! | slot              | holds                                              |
//! |-------------------|----------------------------------------------------|
//! | `regs[-BUDGET]`   | back edges left before tiering up (an `i64`)       |
//! | `regs[-OSR_PC]`, `regs[-OSR_ACC]` | where and with what the interpreter left to tier up |
//! | `regs[-CODE]`     | the running function's [`Code`] (register count, nested functions) |
//! | `regs[-ICS]`      | its inline caches, kept apart to save a load       |
//! | `regs[-VM]`       | the [`Vm`]                                         |
//! | `regs[-RET_PC]`   | where the caller resumes, or null to return to the host |
//! | `regs[-CALLER]`   | the caller's `regs`                                |
//! | `regs[-FLAGS]`    | [`CONSTRUCT`] for a `new` frame                    |
//!
//! Then `r0` (`this`), the parameters, and locals and temporaries, all
//! `undefined` on entry. VM-wide state lives here rather than in a seventh
//! handler argument, which x86-64 couldn't keep in a register.
//!
//! A call copies `this` and the arguments into a new frame just past the
//! caller's registers and tail-calls the callee's first handler with the
//! new `regs`. `Return` tail-calls the caller's next handler with the
//! caller's `regs`. Neither touches the native stack, so JS recursion
//! depth is bounded by the VM stack, not Rust's.

use crate::bytecode::Op;
use crate::jit::VmRun;
use crate::object::{Class, Object, as_object, object_value};
use crate::value::{NumberTag, Value};
use crate::vm::{Code, Error, IC_WAYS, Ic, LENGTH_LOC, VIA_CALL, Vm, string_units};

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

/// Read an unaligned little-endian `u16` operand at `pc + off`.
#[inline(always)]
unsafe fn u16_at(pc: *const u8, off: usize) -> u16 {
    unsafe { u16::from_le(pc.add(off).cast::<u16>().read_unaligned()) }
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
        #[rustc_align(64)]
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
            // Dispatch to `p` in ANY with register file `r` (calls, returns).
            macro_rules! jump_to {
                ($p:expr, $a:expr, $r:expr) => { dispatch!(tbl, $p, 0, $a, $r, $tag, tbl, ti32) };
            }
            // Enter bytecode function object `fo` in a new frame: in its
            // compiled code, once its call countdown has run out.
            macro_rules! enter {
                ($fo:expr, $this:expr, $args:expr, $argc:expr, $flags:expr, $size:expr) => {{
                    let callee: *const Code = (*$fo).code;
                    let new = push_frame($regs, $fo, callee, $this, $args, $argc, $flags, $pc.add($size));
                    if new.is_null() {
                        return stack_overflow($regs);
                    }
                    let left = (*callee).calls_left.get() - 1;
                    (*callee).calls_left.set(left);
                    if left < 0 {
                        let e = enter_compiled(new);
                        match e.kind {
                            ENTER_RETURNED => next!(any, $size, e.acc),
                            ENTER_RESUME => jump_to!(e.pc, e.acc, e.regs),
                            ENTER_ERROR => tail!(op_unwind, Value::EMPTY),
                            _ => {}
                        }
                    }
                    jump_to!((*callee).bytecode.as_ptr(), Value::UNDEFINED, new)
                }};
            }
            // Continue after this instruction with `r`, or end the run on an error.
            macro_rules! next_or_throw {
                ($o:expr, $r:expr) => {
                    match $r {
                        Ok(v) => next!(any, $o, v),
                        Err(e) => {
                            (*frame_vm($regs)).error = Some(e);
                            tail!(op_unwind, Value::EMPTY)
                        }
                    }
                };
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
        // The back edges this frame didn't use go back to the function
        // (written only if it used some: most frames run no loop).
        let code: *const Code = header(regs, CODE);
        if !code.is_null() {
            let left = (*regs.sub(BUDGET)).to_bits() as i64;
            if left != (*code).loop_budget.get() {
                (*code).loop_budget.set(left);
            }
        }
        let flags = (*regs.sub(FLAGS)).to_bits();
        if flags != 0 {
            if flags & CAPTURED != 0 {
                (*frame_vm(regs)).close_upvalues(regs);
            }
            if flags & CONSTRUCT != 0 {
                acc = construct_result(regs, acc);
            }
        }
        let ret: *const u8 = header(regs, RET_PC);
        if ret.is_null() {
            return acc;
        }
        jump_to!(ret, acc, header::<Value>(regs, CALLER))
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
        tail!(op_return, tag.rebox(acc))
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
            None => generic_add(rhs, acc),
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

handlers! {
    // ---- Constants and registers ----
    // The loads ignore `acc`, so they also serve as I32 entries.

    fn op_throw(pc, acc, regs, tag) {
        (*frame_vm(regs)).error = Some(Error::thrown(acc));
        tail!(op_unwind, acc)
    }

    /// The error in `vm.error` was raised at `pc` of frame `regs`: continue
    /// at the innermost handler covering it, in this frame or a caller's
    /// (at its call), with the exception's value in `acc`. A frame entered
    /// from native code (no return pc: the script's, or one compiled code
    /// or a native called) ends the walk: the error goes back to that
    /// caller as `EMPTY`, which unwinds from its own frame.
    #[cold]
    fn op_unwind(pc, acc, regs, tag) {
        let vm = frame_vm(regs);
        let (mut pc, mut regs) = (pc, regs);
        loop {
            let code: *const Code = header(regs, CODE);
            if code.is_null() {
                return Value::EMPTY;
            }
            let base = (*code).bytecode.as_ptr();
            let at = pc.offset_from(base) as u32;
            if let Some(&(_, _, h)) = (&(*code).handlers)
                .iter()
                .find(|&&(start, end, _)| start <= at && at < end)
            {
                let e = (*vm).error.take().expect("an error to unwind");
                let v = match e.1 {
                    Some(v) => v,
                    None => crate::builtins::caught(&mut *vm, &e),
                };
                jump_to!(base.add(h as usize), v, regs)
            }
            if (*regs.sub(FLAGS)).to_bits() & CAPTURED != 0 {
                (*vm).close_upvalues(regs);
            }
            let ret: *const u8 = header(regs, RET_PC);
            if ret.is_null() {
                return Value::EMPTY;
            }
            // Inside the caller's call instruction.
            (pc, regs) = (ret.sub(1), header::<Value>(regs, CALLER));
        }
    }

    fn op_lda_undefined(pc, acc, regs, tag) {
        next!(any, Op::LdaUndefined.size(), Value::UNDEFINED)
    }

    // The arguments the frame's maker passed: those of the caller's call
    // instruction (one on for `f.call(that, …)`, `list`'s elements for
    // `f.apply(that, list)`), or the runtime's (`Vm::pending_args`).
    fn op_create_arguments(pc, acc, regs, tag) {
        let vm = &mut *frame_vm(regs);
        let ret: *const u8 = header(regs, RET_PC);
        let caller: *mut Value = header(regs, CALLER);
        let spread;
        let (args, argc): (*const Value, usize) = if ret.is_null() || caller.is_null() {
            vm.pending_args
        } else {
            let call = ret.sub(Op::Call.size());
            let (f, w, n) = (*call.add(1) as usize, *call.add(2) as usize, *call.add(3) as usize);
            match Op::from_u8(*call) {
                Some(Op::Call) => {
                    let fv = *caller.add(f);
                    let this = *caller.add(w);
                    if unwrap_call(vm, fv, this).is_some() {
                        (caller.add(w + 2).cast_const(), n.saturating_sub(1))
                    } else if unwrap_apply(vm, fv, this).is_some() {
                        let list = if n < 2 { Value::UNDEFINED } else { *caller.add(w + 2) };
                        spread = crate::builtins::apply_list(list).unwrap_or_default();
                        (spread.as_ptr(), spread.len())
                    } else {
                        (caller.add(w + 1).cast_const(), n)
                    }
                }
                _ => (caller.add(w).cast_const(), n),
            }
        };
        let values = std::slice::from_raw_parts(args, argc);
        *reg(pc, 1, regs) = crate::builtins::arguments_object(vm, values);
        next!(any, Op::CreateArguments.size(), acc)
    }

    fn op_test_instance_of(pc, acc, regs, tag) {
        let r = (*frame_vm(regs)).instance_of(*reg(pc, 1, regs), acc);
        next_or_throw!(Op::TestInstanceOf.size(), r.map(Value::from_bool))
    }

    fn op_test_in(pc, acc, regs, tag) {
        let r = (*frame_vm(regs)).has_property(acc, *reg(pc, 1, regs));
        next_or_throw!(Op::TestIn.size(), r.map(Value::from_bool))
    }

    fn op_delete_property(pc, acc, regs, tag) {
        let r = (*frame_vm(regs)).delete_property(*reg(pc, 1, regs), acc);
        next_or_throw!(Op::DeleteProperty.size(), r.map(Value::from_bool))
    }

    fn op_delete_global(pc, acc, regs, tag) {
        let vm = &mut *frame_vm(regs);
        let name = String::from_utf16_lossy(string_units(acc).unwrap_or(&[]));
        let a = vm.intern(&name);
        let r = vm.delete_named(vm.global, a);
        next!(any, Op::DeleteGlobal.size(), Value::from_bool(r))
    }

    fn op_lda_callee(pc, acc, regs, tag) {
        let f: *mut Object = header(regs, FUNC);
        next!(any, Op::LdaCallee.size(), object_value(f))
    }

    fn op_define_accessor(pc, acc, regs, tag) {
        let vm = &mut *frame_vm(regs);
        let o = as_object(*reg(pc, 1, regs));
        let k = u16::from_le_bytes([*pc.add(2), *pc.add(3)]) as usize;
        let name = (&(*frame_code(regs)).constants)[k];
        let name = String::from_utf16_lossy(string_units(name).unwrap_or(&[]));
        let a = vm.intern(&name);
        let (g, s) = if *pc.add(4) == 0 { (Some(acc), None) } else { (None, Some(acc)) };
        vm.define_accessor(o, a, g, s, 0);
        next!(any, Op::DefineAccessor.size(), acc)
    }

    fn op_create_regexp(pc, acc, regs, tag) {
        let vm = &mut *frame_vm(regs);
        let k = |at: usize| (&(*frame_code(regs)).constants)[u16::from_le_bytes([*pc.add(at), *pc.add(at + 1)]) as usize];
        let units = |v: Value| String::from_utf16_lossy(string_units(v).unwrap_or(&[]));
        let (p, f) = (units(k(1)), units(k(3)));
        let r = crate::builtins::new_regexp(vm, &p, &f);
        next_or_throw!(Op::CreateRegExp.size(), r)
    }

    fn op_lda_null(pc, acc, regs, tag) {
        next!(any, Op::LdaNull.size(), Value::NULL)
    }

    fn op_lda_true(pc, acc, regs, tag) {
        next!(any, Op::LdaTrue.size(), Value::TRUE)
    }

    fn op_lda_false(pc, acc, regs, tag) {
        next!(any, Op::LdaFalse.size(), Value::FALSE)
    }

    fn op_mov(pc, acc, regs, tag) {
        *reg(pc, 2, regs) = *reg(pc, 1, regs);
        next!(any, Op::Mov.size(), acc)
    }

    fn i32_mov(pc, acc, regs, tag) {
        *reg(pc, 2, regs) = *reg(pc, 1, regs);
        next!(i32, Op::Mov.size(), acc)
    }

    // ---- Arithmetic ----

    // Non-negative dividends only: a negative one with a zero remainder
    // gives -0, a double.
    fn op_mod_smi(pc, acc, regs, tag) {
        let d = imm32(pc, 1);
        if tag.is_int32(acc) && d > 0 && acc.as_i32_unchecked() >= 0 {
            next_int!(Op::ModSmi.size(), acc.as_i32_unchecked() % d)
        }
        tail!(slow_mod_smi, acc)
    }

    fn i32_mod_smi(pc, acc, regs, tag) {
        let d = imm32(pc, 1);
        if d > 0 && raw_i32(acc) >= 0 {
            next!(i32, Op::ModSmi.size(), unboxed(raw_i32(acc) % d))
        }
        tail!(slow_mod_smi, tag.rebox(acc))
    }

    fn op_bit_and_smi(pc, acc, regs, tag) {
        if tag.is_int32(acc) {
            next_int!(Op::BitAndSmi.size(), acc.as_i32_unchecked() & imm32(pc, 1))
        }
        tail!(slow_bit_and_smi, acc)
    }

    fn i32_bit_and_smi(pc, acc, regs, tag) {
        next!(i32, Op::BitAndSmi.size(), unboxed(raw_i32(acc) & imm32(pc, 1)))
    }

    fn op_bit_or_smi(pc, acc, regs, tag) {
        if tag.is_int32(acc) {
            next_int!(Op::BitOrSmi.size(), acc.as_i32_unchecked() | imm32(pc, 1))
        }
        tail!(slow_bit_or_smi, acc)
    }

    fn i32_bit_or_smi(pc, acc, regs, tag) {
        next!(i32, Op::BitOrSmi.size(), unboxed(raw_i32(acc) | imm32(pc, 1)))
    }

    // ---- Comparisons: the result is a boolean, in ANY ----

    fn op_test_less_than(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if tag.both_int32(lhs, acc) {
            let r = lhs.as_i32_unchecked() < acc.as_i32_unchecked();
            next!(any, Op::TestLessThan.size(), Value::from_bool(r))
        }
        tail!(slow_test_less_than, acc)
    }

    fn i32_test_less_than(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if tag.is_int32(lhs) {
            let r = lhs.as_i32_unchecked() < raw_i32(acc);
            next!(any, Op::TestLessThan.size(), Value::from_bool(r))
        }
        tail!(slow_test_less_than, tag.rebox(acc))
    }

    fn op_test_equal(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if tag.both_int32(lhs, acc) {
            let r = lhs.as_i32_unchecked() == acc.as_i32_unchecked();
            next!(any, Op::TestEqual.size(), Value::from_bool(r))
        }
        // The same value, unless it's a double (NaN != NaN): the same object,
        // null, undefined or boolean.
        if lhs == acc && !tag.is_double(acc) {
            next!(any, Op::TestEqual.size(), Value::TRUE)
        }
        tail!(slow_test_equal, acc)
    }

    fn op_test_equal_smi(pc, acc, regs, tag) {
        if tag.is_int32(acc) {
            let r = acc.as_i32_unchecked() == imm32(pc, 1);
            next!(any, Op::TestEqualSmi.size(), Value::from_bool(r))
        }
        tail!(slow_test_equal_smi, acc)
    }

    fn i32_test_equal_smi(pc, acc, regs, tag) {
        let r = raw_i32(acc) == imm32(pc, 1);
        next!(any, Op::TestEqualSmi.size(), Value::from_bool(r))
    }

    // ---- Conditional jumps ----

    fn op_jump_if_true(pc, acc, regs, tag) {
        if acc == Value::TRUE {
            next!(any, imm32(pc, 1), acc)
        }
        next!(any, Op::JumpIfTrue.size(), acc)
    }

    fn op_jump_if_false(pc, acc, regs, tag) {
        if acc == Value::FALSE {
            next!(any, imm32(pc, 1), acc)
        }
        next!(any, Op::JumpIfFalse.size(), acc)
    }

    fn op_jump_if_to_boolean_true(pc, acc, regs, tag) {
        if acc == Value::TRUE {
            next!(any, imm32(pc, 1), acc)
        }
        if acc == Value::FALSE {
            next!(any, Op::JumpIfToBooleanTrue.size(), acc)
        }
        tail!(slow_jump_if_to_boolean_true, acc)
    }

    fn i32_jump_if_to_boolean_true(pc, acc, regs, tag) {
        if raw_i32(acc) != 0 {
            next!(i32, imm32(pc, 1), acc)
        }
        next!(i32, Op::JumpIfToBooleanTrue.size(), acc)
    }

    fn op_jump_if_to_boolean_false(pc, acc, regs, tag) {
        if acc == Value::FALSE {
            next!(any, imm32(pc, 1), acc)
        }
        if acc == Value::TRUE {
            next!(any, Op::JumpIfToBooleanFalse.size(), acc)
        }
        tail!(slow_jump_if_to_boolean_false, acc)
    }

    fn i32_jump_if_to_boolean_false(pc, acc, regs, tag) {
        if raw_i32(acc) == 0 {
            next!(i32, imm32(pc, 1), acc)
        }
        next!(i32, Op::JumpIfToBooleanFalse.size(), acc)
    }

    // ---- Properties ----
    // The hit path: check the receiver is a cell, compare its shape with
    // the cache's, load or store at the cached location. Loads ignore
    // `acc`, so they also serve as I32 entries.

    fn op_lda_global(pc, acc, regs, tag) {
        let g = (*frame_vm(regs)).global;
        let ic = ic_at(regs, pc, 1);
        if (*g).shape == (*ic).shape && (*ic).holder.is_null() {
            next!(any, Op::LdaGlobal.size(), *Object::slot(g, (*ic).loc))
        }
        tail!(slow_lda_global, acc)
    }

    fn op_lda_global_inside_typeof(pc, acc, regs, tag) {
        let g = (*frame_vm(regs)).global;
        let ic = ic_at(regs, pc, 1);
        if (*g).shape == (*ic).shape && (*ic).holder.is_null() {
            next!(any, Op::LdaGlobalInsideTypeof.size(), *Object::slot(g, (*ic).loc))
        }
        let r = (*frame_vm(regs)).get_global_typeof_miss(&*frame_code(regs), u16_at(pc, 1));
        next_or_throw!(Op::LdaGlobalInsideTypeof.size(), r)
    }

    fn op_lda_upvalue(pc, acc, regs, tag) {
        let f: *const crate::object::Object = header(regs, FUNC);
        let u = *Vm::upvalues(f).add(*pc.add(1) as usize);
        next!(any, Op::LdaUpvalue.size(), *(*u).at)
    }

    fn op_sta_upvalue(pc, acc, regs, tag) {
        let f: *const crate::object::Object = header(regs, FUNC);
        let u = *Vm::upvalues(f).add(*pc.add(1) as usize);
        *(*u).at = acc;
        next!(any, Op::StaUpvalue.size(), acc)
    }

    // `r OP acc` for `%` and the bitwise operators, on any operands (the
    // generic forms: the `…Smi` ones take a constant).
    fn op_mod(pc, acc, regs, tag) {
        let (a, b) = (*reg(pc, 1, regs), acc);
        let r = if tag.both_int32(a, b) && b.as_i32_unchecked() > 0 && a.as_i32_unchecked() >= 0 {
            Value::from_i32(a.as_i32_unchecked() % b.as_i32_unchecked())
        } else {
            Value::from_f64_arith(to_number(a) % to_number(b))
        };
        next!(any, Op::Mod.size(), r)
    }

    fn op_bit_and(pc, acc, regs, tag) {
        next_int!(Op::BitAnd.size(), int32_of(*reg(pc, 1, regs)) & int32_of(acc))
    }

    fn op_bit_or(pc, acc, regs, tag) {
        next_int!(Op::BitOr.size(), int32_of(*reg(pc, 1, regs)) | int32_of(acc))
    }

    fn op_bit_xor(pc, acc, regs, tag) {
        next_int!(Op::BitXor.size(), int32_of(*reg(pc, 1, regs)) ^ int32_of(acc))
    }

    fn op_shift_left(pc, acc, regs, tag) {
        let n = int32_of(acc) as u32 & 31;
        next_int!(Op::ShiftLeft.size(), int32_of(*reg(pc, 1, regs)).wrapping_shl(n))
    }

    fn op_shift_right(pc, acc, regs, tag) {
        let n = int32_of(acc) as u32 & 31;
        next_int!(Op::ShiftRight.size(), int32_of(*reg(pc, 1, regs)) >> n)
    }

    fn op_shift_right_logical(pc, acc, regs, tag) {
        let n = int32_of(acc) as u32 & 31;
        let r = (int32_of(*reg(pc, 1, regs)) as u32) >> n;
        next!(any, Op::ShiftRightLogical.size(), Value::number(r as f64))
    }

    fn op_to_number(pc, acc, regs, tag) {
        if acc.is_number() {
            next!(any, Op::ToNumber.size(), acc)
        }
        next!(any, Op::ToNumber.size(), Value::number(to_number(acc)))
    }

    fn op_negate(pc, acc, regs, tag) {
        // (-0 is a double: `-x` of int32 0 isn't 0.)
        if tag.is_int32(acc) && acc.as_i32_unchecked() != 0 && acc.as_i32_unchecked() != i32::MIN {
            next_int!(Op::Negate.size(), -acc.as_i32_unchecked())
        }
        next!(any, Op::Negate.size(), Value::from_f64(-to_number(acc)))
    }

    fn op_bit_not(pc, acc, regs, tag) {
        next_int!(Op::BitNot.size(), !int32_of(acc))
    }

    fn op_type_of(pc, acc, regs, tag) {
        let v = (*frame_vm(regs)).type_of(acc);
        next!(any, Op::TypeOf.size(), v)
    }

    fn op_sta_global(pc, acc, regs, tag) {
        let g = (*frame_vm(regs)).global;
        let ic = ic_at(regs, pc, 1);
        if (*g).shape == (*ic).shape && fits_field(tag, (*ic).holder as u64, acc) {
            *Object::slot(g, (*ic).loc) = acc;
            next!(any, Op::StaGlobal.size(), acc)
        }
        tail!(slow_sta_global, acc)
    }

    fn op_get_named(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        if is_cell(tag, obj) {
            let o = as_object(obj);
            let ic = ic_at(regs, pc, 2);
            if (*o).shape == (*ic).shape {
                let holder = (*ic).holder;
                if holder.is_null() {
                    next!(any, Op::GetNamed.size(), *Object::slot(o, (*ic).loc))
                }
                // A prototype's property: valid while no prototype changed shape.
                if (*ic).epoch == (*frame_vm(regs)).proto_epoch {
                    next!(any, Op::GetNamed.size(), *Object::slot(holder, (*ic).loc))
                }
            }
            tail!(poly_get_named, acc)
        }
        tail!(slow_get_named, acc)
    }

    // Way 0 missed: `length`, or the other ways (the receiver is a cell).
    // The ways stay in place: moving a hit to way 0 (64 bytes swapped) cost
    // a site alternating between shapes more than it saved one whose
    // receivers changed shape for good.
    #[inline(never)]
    fn poly_get_named(pc, acc, regs, tag) {
        let o = as_object(*reg(pc, 1, regs));
        let ic = ic_at(regs, pc, 2);
        // An array's or a string's `length` (cached without a shape).
        if (*ic).loc == LENGTH_LOC && matches!((*o).class, Class::Array | Class::String) {
            next!(any, Op::GetNamed.size(), Value::from_i32((*o).elems_len as i32))
        }
        for k in 1..IC_WAYS {
            let w = ic.add(k);
            if (*o).shape == (*w).shape {
                let holder = (*w).holder;
                if holder.is_null() {
                    next!(any, Op::GetNamed.size(), *Object::slot(o, (*w).loc))
                }
                if (*w).epoch == (*frame_vm(regs)).proto_epoch {
                    next!(any, Op::GetNamed.size(), *Object::slot(holder, (*w).loc))
                }
                break;
            }
        }
        tail!(slow_get_named, acc)
    }

    // The externalization barrier will go here (step 3 of the object-model
    // plan); every object is external for now.
    fn op_set_named(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        if is_cell(tag, obj) {
            let o = as_object(obj);
            let ic = ic_at(regs, pc, 2);
            if (*o).shape == (*ic).shape && fits_field(tag, (*ic).holder as u64, acc) {
                let new = (*ic).new_shape;
                if !new.is_null() {
                    (*o).shape = new;
                }
                *Object::slot(o, (*ic).loc) = acc;
                next!(any, Op::SetNamed.size(), acc)
            }
            tail!(poly_set_named, acc)
        }
        tail!(slow_set_named, acc)
    }

    // Way 0 missed: the other ways (the receiver is a cell, `acc` boxed),
    // as for loads.
    #[inline(never)]
    fn poly_set_named(pc, acc, regs, tag) {
        let o = as_object(*reg(pc, 1, regs));
        let ic = ic_at(regs, pc, 2);
        for k in 1..IC_WAYS {
            let w = ic.add(k);
            if (*o).shape == (*w).shape {
                if fits_field(tag, (*w).holder as u64, acc) {
                    let new = (*w).new_shape;
                    if !new.is_null() {
                        (*o).shape = new;
                    }
                    *Object::slot(o, (*w).loc) = acc;
                    next!(any, Op::SetNamed.size(), acc)
                }
                break;
            }
        }
        tail!(slow_set_named, acc)
    }

    // `acc` is an int32: it fits any field type up to a number (encoded 0,
    // 1, 2), so the check is one compare. Also saves `i32_to_any`'s second
    // dispatch.
    fn i32_set_named(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        let acc = tag.rebox(acc);
        if is_cell(tag, obj) {
            let o = as_object(obj);
            let ic = ic_at(regs, pc, 2);
            if (*o).shape == (*ic).shape && ((*ic).holder as u64) <= 2 {
                let new = (*ic).new_shape;
                if !new.is_null() {
                    (*o).shape = new;
                }
                *Object::slot(o, (*ic).loc) = acc;
                next!(any, Op::SetNamed.size(), acc)
            }
            tail!(poly_set_named, acc)
        }
        tail!(slow_set_named, acc)
    }

    // The receiver's shape must be the cached one: the JIT reads the cache
    // for the elements' type. A miss records the new shape.
    fn op_get_keyed(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        if is_cell(tag, obj) && tag.is_int32(acc) {
            let o = as_object(obj);
            let i = acc.as_i32_unchecked() as u32;
            if (*o).shape == (*ic_at(regs, pc, 2)).shape && i < (*o).elems_len {
                let v = *(*o).elems.add(i as usize);
                if v != Value::EMPTY {
                    next!(any, Op::GetKeyed.size(), v)
                }
            }
        }
        tail!(slow_get_keyed, acc)
    }

    fn i32_get_keyed(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        if is_cell(tag, obj) {
            let o = as_object(obj);
            let i = raw_i32(acc) as u32;
            if (*o).shape == (*ic_at(regs, pc, 2)).shape && i < (*o).elems_len {
                let v = *(*o).elems.add(i as usize);
                if v != Value::EMPTY {
                    next!(any, Op::GetKeyed.size(), v)
                }
            }
        }
        tail!(slow_get_keyed, tag.rebox(acc))
    }

    fn op_set_keyed(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        let key = *reg(pc, 2, regs);
        if is_cell(tag, obj) && tag.is_int32(key) {
            let o = as_object(obj);
            let i = key.as_i32_unchecked() as u32;
            if i < (*o).elems_len && fits_field(tag, (*(*o).shape).elems_code, acc) {
                *(*o).elems.add(i as usize) = acc;
                next!(any, Op::SetKeyed.size(), acc)
            }
        }
        tail!(slow_set_keyed, acc)
    }

    // ---- Allocation (ignore `acc`: also I32 entries) ----

    fn op_create_closure(pc, acc, regs, tag) {
        let f = create_closure(regs, u16_at(pc, 1));
        next!(any, Op::CreateClosure.size(), f)
    }

    fn op_create_array(pc, acc, regs, tag) {
        let a = new_array(regs);
        next!(any, Op::CreateArray.size(), a)
    }

    // Ignores `acc`: also the I32 entry.
    fn op_create_object(pc, acc, regs, tag) {
        let o = create_object(regs, u16_at(pc, 2), *pc.add(1) as u32);
        next!(any, Op::CreateObject.size(), o)
    }

    fn op_for_in_keys(pc, acc, regs, tag) {
        let keys = (*frame_vm(regs)).for_in_keys(acc);
        next!(any, Op::ForInKeys.size(), keys)
    }

    // Ignores `acc`: also the I32 entry.
    fn op_lda_constant(pc, acc, regs, tag) {
        let code = &*frame_code(regs);
        let c = *code.constants.get_unchecked(u16_at(pc, 1) as usize);
        next!(any, Op::LdaConstant.size(), c)
    }

    // ---- Calls (ignore `acc`: also I32 entries) ----
    // A call records its callee in the call's inline cache (`holder`), which
    // the JIT inlines from (natives in the slow paths).

    fn op_call(pc, acc, regs, tag) {
        let f = *reg(pc, 1, regs);
        if is_cell(tag, f) && (*as_object(f)).class == Class::Function {
            note_callee(ic_at(regs, pc, 4), as_object(f));
            let w = reg(pc, 2, regs);
            enter!(as_object(f), *w, w.add(1), *pc.add(3) as usize, 0, Op::Call.size())
        }
        tail!(slow_call, acc)
    }

    fn op_call_undefined(pc, acc, regs, tag) {
        let f = *reg(pc, 1, regs);
        if is_cell(tag, f) && (*as_object(f)).class == Class::Function {
            note_callee(ic_at(regs, pc, 4), as_object(f));
            let args = reg(pc, 2, regs);
            // Sloppy mode (toi has no strict mode): `this` is the global
            // object.
            let this = object_value((*frame_vm(regs)).global);
            enter!(as_object(f), this, args, *pc.add(3) as usize, 0, Op::CallUndefined.size())
        }
        tail!(slow_call_undefined, acc)
    }

    fn op_construct(pc, acc, regs, tag) {
        let f = *reg(pc, 1, regs);
        if is_cell(tag, f) && (*as_object(f)).class == Class::Function {
            note_callee(ic_at(regs, pc, 4), as_object(f));
            let this = construct_this(regs, as_object(f));
            let args = reg(pc, 2, regs);
            enter!(as_object(f), this, args, *pc.add(3) as usize, CONSTRUCT, Op::Construct.size())
        }
        tail!(slow_construct, acc)
    }

    // ---- Slow paths ----

    #[cold]
    #[inline(never)]
    fn slow_mod_smi(pc, acc, regs, tag) {
        let d = imm32(pc, 1) as f64;
        let r = match acc.as_number() {
            Some(a) => Value::number(a % d),
            None => Value::number(to_number(acc) % d),
        };
        next!(any, Op::ModSmi.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_bit_and_smi(pc, acc, regs, tag) {
        next_int!(Op::BitAndSmi.size(), to_int32(to_number(acc)) & imm32(pc, 1))
    }

    #[cold]
    #[inline(never)]
    fn slow_bit_or_smi(pc, acc, regs, tag) {
        next_int!(Op::BitOrSmi.size(), to_int32(to_number(acc)) | imm32(pc, 1))
    }

    #[cold]
    #[inline(never)]
    fn slow_test_less_than(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        let r = match both_numbers(lhs, acc) {
            Some((a, b)) => a < b,
            None => generic_less_than(lhs, acc),
        };
        next!(any, Op::TestLessThan.size(), Value::from_bool(r))
    }

    #[cold]
    #[inline(never)]
    fn slow_test_equal(pc, acc, regs, tag) {
        let r = loose_equals(*reg(pc, 1, regs), acc).map(Value::from_bool);
        next_or_throw!(Op::TestEqual.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_test_equal_smi(pc, acc, regs, tag) {
        let r = loose_equals(acc, Value::from_i32(imm32(pc, 1))).map(Value::from_bool);
        next_or_throw!(Op::TestEqualSmi.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_to_boolean_true(pc, acc, regs, tag) {
        if to_boolean(acc) {
            next!(any, imm32(pc, 1), acc)
        }
        next!(any, Op::JumpIfToBooleanTrue.size(), acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_to_boolean_false(pc, acc, regs, tag) {
        if !to_boolean(acc) {
            next!(any, imm32(pc, 1), acc)
        }
        next!(any, Op::JumpIfToBooleanFalse.size(), acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_lda_global(pc, acc, regs, tag) {
        let r = (*frame_vm(regs)).get_global_miss(&*frame_code(regs), u16_at(pc, 1));
        next_or_throw!(Op::LdaGlobal.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_sta_global(pc, acc, regs, tag) {
        let r = (*frame_vm(regs)).set_global_miss(&*frame_code(regs), u16_at(pc, 1), acc);
        next_or_throw!(Op::StaGlobal.size(), r.map(|()| acc))
    }

    #[cold]
    #[inline(never)]
    fn slow_get_named(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        let r = with_top(regs, None, |vm| vm.get_named_miss(&*frame_code(regs), u16_at(pc, 2), obj));
        next_or_throw!(Op::GetNamed.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_set_named(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        let r = with_top(regs, None, |vm| vm.set_named_miss(&*frame_code(regs), u16_at(pc, 2), obj, acc));
        next_or_throw!(Op::SetNamed.size(), r.map(|()| acc))
    }

    #[cold]
    #[inline(never)]
    fn slow_get_keyed(pc, acc, regs, tag) {
        let obj = *reg(pc, 1, regs);
        let r = with_top(regs, None, |vm| vm.get_keyed_miss(&*frame_code(regs), u16_at(pc, 2), obj, acc));
        next_or_throw!(Op::GetKeyed.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_set_keyed(pc, acc, regs, tag) {
        let r = with_top(regs, None, |vm| vm.set_keyed(*reg(pc, 1, regs), *reg(pc, 2, regs), acc));
        next_or_throw!(Op::SetKeyed.size(), r.map(|()| acc))
    }

    #[cold]
    #[inline(never)]
    fn slow_call(pc, acc, regs, tag) {
        let w = reg(pc, 2, regs);
        // `f.call(that, …args)` with `f` a bytecode function: call `f`,
        // with the window one register on. The cache records `f`, for the
        // JIT to call (`VIA_CALL`).
        let argc = *pc.add(3) as usize;
        let ic = ic_at(regs, pc, 4);
        if let Some(fo) = unwrap_call(frame_vm(regs), *reg(pc, 1, regs), *w) {
            note_callee(ic, fo);
            (*ic).loc = VIA_CALL;
            let that = if argc == 0 { Value::UNDEFINED } else { *w.add(1) };
            let that = sloppy_this(frame_vm(regs), that);
            enter!(fo, that, w.add(2), argc.saturating_sub(1), 0, Op::Call.size())
        }
        // `f.apply(that, list)` with `f` a bytecode function: call `f` with
        // `list`'s elements. The cache records `apply` (compiled code calls
        // it through the runtime, which unwraps it too).
        if let Some(fo) = unwrap_apply(frame_vm(regs), *reg(pc, 1, regs), *w) {
            record_callee(pc, regs, tag);
            (*ic).loc = 0;
            let that = if argc == 0 { Value::UNDEFINED } else { *w.add(1) };
            let that = sloppy_this(frame_vm(regs), that);
            let list = if argc < 2 { Value::UNDEFINED } else { *w.add(2) };
            match crate::builtins::apply_list(list) {
                Ok(spread) => enter!(fo, that, spread.as_ptr(), spread.len(), 0, Op::Call.size()),
                Err(e) => next_or_throw!(Op::Call.size(), Err::<Value, Error>(e)),
            }
        }
        record_callee(pc, regs, tag);
        (*ic).loc = 0;
        let args = std::slice::from_raw_parts(w.add(1), *pc.add(3) as usize);
        let r = with_top(regs, None, |vm| vm.call_other(*reg(pc, 1, regs), *w, args));
        next_or_throw!(Op::Call.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_call_undefined(pc, acc, regs, tag) {
        record_callee(pc, regs, tag);
        let args = std::slice::from_raw_parts(reg(pc, 2, regs), *pc.add(3) as usize);
        let r = with_top(regs, None, |vm| vm.call_other(*reg(pc, 1, regs), Value::UNDEFINED, args));
        next_or_throw!(Op::CallUndefined.size(), r)
    }

    #[cold]
    #[inline(never)]
    fn slow_construct(pc, acc, regs, tag) {
        record_callee(pc, regs, tag);
        let args = std::slice::from_raw_parts(reg(pc, 2, regs), *pc.add(3) as usize);
        let r = with_top(regs, None, |vm| vm.construct_other(*reg(pc, 1, regs), args));
        next_or_throw!(Op::Construct.size(), r)
    }
}

handlers! {
    // ---- Superinstructions: a comparison and the conditional jump after it ----
    // The jump's offset is at `pc + n + 1`, where `n` is the comparison's
    // size, and is relative to the jump (`pc + n`). Slow paths are the
    // comparison's own: they leave the boolean in `acc` and dispatch to the
    // jump instruction, which is still there.

    fn op_test_less_than_jump_if_true(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if !tag.both_int32(lhs, acc) {
            tail!(slow_test_less_than, acc)
        }
        let c = lhs.as_i32_unchecked() < acc.as_i32_unchecked();
        if c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::TRUE)
        }
        next!(any, 2 + Op::JumpIfTrue.size(), Value::FALSE)
    }

    fn i32_test_less_than_jump_if_true(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if !tag.is_int32(lhs) {
            tail!(slow_test_less_than, tag.rebox(acc))
        }
        let c = lhs.as_i32_unchecked() < raw_i32(acc);
        if c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::TRUE)
        }
        next!(any, 2 + Op::JumpIfTrue.size(), Value::FALSE)
    }

    fn op_test_equal_jump_if_true(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        let c = if tag.both_int32(lhs, acc) {
            lhs.as_i32_unchecked() == acc.as_i32_unchecked()
        } else if lhs == acc && !tag.is_double(acc) {
            true
        } else {
            tail!(slow_test_equal, acc)
        };
        if c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::TRUE)
        }
        next!(any, 2 + Op::JumpIfTrue.size(), Value::FALSE)
    }

    fn op_test_equal_smi_jump_if_true(pc, acc, regs, tag) {
        if !tag.is_int32(acc) {
            tail!(slow_test_equal_smi, acc)
        }
        let c = acc.as_i32_unchecked() == imm32(pc, 1);
        if c {
            next!(any, 5 + imm32(pc, 5 + 1) as isize, Value::TRUE)
        }
        next!(any, 5 + Op::JumpIfTrue.size(), Value::FALSE)
    }

    fn i32_test_equal_smi_jump_if_true(pc, acc, regs, tag) {
        let c = raw_i32(acc) == imm32(pc, 1);
        if c {
            next!(any, 5 + imm32(pc, 5 + 1) as isize, Value::TRUE)
        }
        next!(any, 5 + Op::JumpIfTrue.size(), Value::FALSE)
    }

    fn op_test_less_than_jump_if_false(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if !tag.both_int32(lhs, acc) {
            tail!(slow_test_less_than, acc)
        }
        let c = lhs.as_i32_unchecked() < acc.as_i32_unchecked();
        if !c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::FALSE)
        }
        next!(any, 2 + Op::JumpIfFalse.size(), Value::TRUE)
    }

    fn i32_test_less_than_jump_if_false(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if !tag.is_int32(lhs) {
            tail!(slow_test_less_than, tag.rebox(acc))
        }
        let c = lhs.as_i32_unchecked() < raw_i32(acc);
        if !c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::FALSE)
        }
        next!(any, 2 + Op::JumpIfFalse.size(), Value::TRUE)
    }

    fn op_test_equal_jump_if_false(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        let c = if tag.both_int32(lhs, acc) {
            lhs.as_i32_unchecked() == acc.as_i32_unchecked()
        } else if lhs == acc && !tag.is_double(acc) {
            true
        } else {
            tail!(slow_test_equal, acc)
        };
        if !c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::FALSE)
        }
        next!(any, 2 + Op::JumpIfFalse.size(), Value::TRUE)
    }

    fn op_test_equal_smi_jump_if_false(pc, acc, regs, tag) {
        if !tag.is_int32(acc) {
            tail!(slow_test_equal_smi, acc)
        }
        let c = acc.as_i32_unchecked() == imm32(pc, 1);
        if !c {
            next!(any, 5 + imm32(pc, 5 + 1) as isize, Value::FALSE)
        }
        next!(any, 5 + Op::JumpIfFalse.size(), Value::TRUE)
    }

    fn i32_test_equal_smi_jump_if_false(pc, acc, regs, tag) {
        let c = raw_i32(acc) == imm32(pc, 1);
        if !c {
            next!(any, 5 + imm32(pc, 5 + 1) as isize, Value::FALSE)
        }
        next!(any, 5 + Op::JumpIfFalse.size(), Value::TRUE)
    }
}

// ---- Group 2: arithmetic, comparisons, compare-and-jumps ----

/// `r OP acc` (or `acc OP imm`) for any two values: as numbers, there being
/// no strings yet. The result is normalized like the other slow paths'.
#[cold]
#[inline(never)]
pub(crate) fn generic_arith(op: Op, a: Value, b: Value) -> Value {
    let (x, y) = match both_numbers(a, b) {
        Some(p) => p,
        None => (to_number(a), to_number(b)),
    };
    Value::from_f64_arith(arith_f64(op, x, y))
}

/// `x OP y` for `Sub`, `Mul`, `Div` (or their `…Smi` forms).
#[inline(always)]
fn arith_f64(op: Op, x: f64, y: f64) -> f64 {
    match op {
        Op::Sub | Op::SubSmi => x - y,
        Op::Mul | Op::MulSmi => x * y,
        _ => x / y,
    }
}

/// `a < b` or `a <= b` (`le`) for any two values; false with NaN.
#[cold]
#[inline(never)]
pub(crate) fn generic_compare(le: bool, a: Value, b: Value) -> bool {
    let (x, y) = match both_numbers(a, b) {
        Some(p) => p,
        None => {
            // Two strings compare by code units.
            let (a, b) = (to_primitive(a), to_primitive(b));
            if let (Some(p), Some(q)) = (string_units(a), string_units(b)) {
                return if le { p <= q } else { p < q };
            }
            (to_number(a), to_number(b))
        }
    };
    if le { x <= y } else { x < y }
}

/// `a === b` (IsStrictlyEqual) for the values that exist so far.
pub(crate) fn strict_equals(a: Value, b: Value) -> bool {
    if let Some((x, y)) = both_numbers(a, b) {
        return x == y;
    }
    match (string_units(a), string_units(b)) {
        (Some(p), Some(q)) => p == q,
        _ => a == b,
    }
}

/// An int32 quotient `a / b`, if it is one: exact, not -0, not 2^31.
#[inline(always)]
fn int_div(a: i32, b: i32) -> Option<i32> {
    if b == 0 || (a == 0 && b < 0) {
        return None;
    }
    let q = a.checked_div(b)?;
    (q * b == a).then_some(q)
}

/// An int32 product `a * b`, if it is one: no overflow, not -0.
#[inline(always)]
fn int_mul(a: i32, b: i32) -> Option<i32> {
    let p = a.checked_mul(b)?;
    (p != 0 || (a | b) >= 0).then_some(p)
}

/// Arithmetic `r OP acc` with a register operand, int32 fast path `$int`.
macro_rules! arith_reg {
    ($op:ident, $any:ident, $i32:ident, $slow:ident, $int:path) => {
        handlers! {
            fn $any(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if tag.both_int32(lhs, acc)
                    && let Some(v) = $int(lhs.as_i32_unchecked(), acc.as_i32_unchecked())
                {
                    next_int!(Op::$op.size(), v)
                }
                tail!($slow, acc)
            }

            fn $i32(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if tag.is_int32(lhs)
                    && let Some(v) = $int(lhs.as_i32_unchecked(), raw_i32(acc))
                {
                    next!(i32, Op::$op.size(), unboxed(v))
                }
                tail!($slow, tag.rebox(acc))
            }

            #[cold]
            #[inline(never)]
            fn $slow(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                let v = match both_numbers(lhs, acc) {
                    Some((x, y)) => Value::from_f64_arith(arith_f64(Op::$op, x, y)),
                    None => generic_arith(Op::$op, lhs, acc),
                };
                next!(any, Op::$op.size(), v)
            }
        }
    };
}

/// Arithmetic `acc OP imm` (`$int(acc, imm)`).
macro_rules! arith_smi {
    ($op:ident, $any:ident, $i32:ident, $slow:ident, $int:path) => {
        handlers! {
            fn $any(pc, acc, regs, tag) {
                if tag.is_int32(acc)
                    && let Some(v) = $int(acc.as_i32_unchecked(), imm32(pc, 1))
                {
                    next_int!(Op::$op.size(), v)
                }
                tail!($slow, acc)
            }

            fn $i32(pc, acc, regs, tag) {
                if let Some(v) = $int(raw_i32(acc), imm32(pc, 1)) {
                    next!(i32, Op::$op.size(), unboxed(v))
                }
                tail!($slow, tag.rebox(acc))
            }

            #[cold]
            #[inline(never)]
            fn $slow(pc, acc, regs, tag) {
                let k = imm32(pc, 1);
                let v = match acc.as_number() {
                    Some(x) => Value::from_f64_arith(arith_f64(Op::$op, x, k as f64)),
                    None => generic_arith(Op::$op, acc, Value::from_i32(k)),
                };
                next!(any, Op::$op.size(), v)
            }
        }
    };
}

arith_reg!(Sub, op_sub, i32_sub, slow_sub, i32::checked_sub);
arith_reg!(Mul, op_mul, i32_mul, slow_mul, int_mul);
arith_reg!(Div, op_div, i32_div, slow_div, int_div);
handlers! {
    // In place too (`sub r13d, eax; jo`); the slow path gets `acc` back
    // as `wrapped + k`, reboxed.
    fn op_sub_smi(pc, acc, regs, tag) {
        if tag.is_int32(acc) {
            let imm = imm32(pc, 1);
            let (v, overflow) = acc.as_i32_unchecked().overflowing_sub(imm);
            if !overflow {
                next_int!(Op::SubSmi.size(), v)
            }
            tail!(slow_sub_smi, tag.box_i32(v.wrapping_add(imm)))
        }
        tail!(slow_sub_smi, acc)
    }

    // In place, as `i32_add_smi`: on overflow, recover `acc` as `wrapped + k`.
    fn i32_sub_smi(pc, acc, regs, tag) {
        let imm = imm32(pc, 1);
        let (v, overflow) = raw_i32(acc).overflowing_sub(imm);
        if overflow {
            tail!(slow_sub_smi, tag.rebox(unboxed(v.wrapping_add(imm))))
        }
        next!(i32, Op::SubSmi.size(), unboxed(v))
    }

    #[cold]
    #[inline(never)]
    fn slow_sub_smi(pc, acc, regs, tag) {
        let v = generic_arith(Op::SubSmi, acc, Value::from_i32(imm32(pc, 1)));
        next!(any, Op::SubSmi.size(), v)
    }
}
arith_smi!(MulSmi, op_mul_smi, i32_mul_smi, slow_mul_smi, int_mul);
arith_smi!(DivSmi, op_div_smi, i32_div_smi, slow_div_smi, int_div);

/// A shift by a constant: on the int32 (after ToInt32), by `imm & 31`.
macro_rules! shift_smi {
    ($op:ident, $any:ident, $i32:ident, $slow:ident, |$a:ident, $k:ident| $int:expr) => {
        handlers! {
            fn $any(pc, acc, regs, tag) {
                if tag.is_int32(acc) {
                    let ($a, $k) = (acc.as_i32_unchecked(), imm32(pc, 1) as u32 & 31);
                    if let Some(v) = $int {
                        next_int!(Op::$op.size(), v)
                    }
                }
                tail!($slow, acc)
            }

            fn $i32(pc, acc, regs, tag) {
                let ($a, $k) = (raw_i32(acc), imm32(pc, 1) as u32 & 31);
                if let Some(v) = $int {
                    next!(i32, Op::$op.size(), unboxed(v))
                }
                tail!($slow, tag.rebox(acc))
            }

            #[cold]
            #[inline(never)]
            fn $slow(pc, acc, regs, tag) {
                let ($a, $k) = (to_int32(to_number(acc)), imm32(pc, 1) as u32 & 31);
                let v = match $int {
                    Some(v) => Value::from_i32(v),
                    // Only `>>>` leaves int32: a uint32 above 2^31 - 1.
                    None => Value::from_f64_arith(($a as u32 >> $k) as f64),
                };
                next!(any, Op::$op.size(), v)
            }
        }
    };
}

shift_smi!(
    ShiftLeftSmi,
    op_shl_smi,
    i32_shl_smi,
    slow_shl_smi,
    |a, k| Some(a << k)
);
shift_smi!(
    ShiftRightSmi,
    op_sar_smi,
    i32_sar_smi,
    slow_sar_smi,
    |a, k| Some(a >> k)
);
shift_smi!(
    ShiftRightLogicalSmi,
    op_shr_smi,
    i32_shr_smi,
    slow_shr_smi,
    |a, k| { i32::try_from(a as u32 >> k).ok() }
);

/// A relational test `r OP acc` (`$le` picks `<=`; `$swap` compares `acc OP
/// r`, so `>` is `acc < r` and `>=` is `acc <= r`), its fused forms with
/// the following `JumpIfTrue` / `JumpIfFalse`, all in both states.
macro_rules! relational {
    ($op:ident, $jt:ident, $jf:ident, $le:literal, $swap:literal,
     $any:ident, $i32:ident, $slow:ident, $any_jt:ident, $any_jf:ident, $i32_jt:ident, $i32_jf:ident) => {
        handlers! {
            fn $any(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if tag.both_int32(lhs, acc) {
                    let c = rel_i32($le, $swap, lhs.as_i32_unchecked(), acc.as_i32_unchecked());
                    next!(any, Op::$op.size(), Value::from_bool(c))
                }
                tail!($slow, acc)
            }

            fn $i32(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if tag.is_int32(lhs) {
                    let c = rel_i32($le, $swap, lhs.as_i32_unchecked(), raw_i32(acc));
                    next!(any, Op::$op.size(), Value::from_bool(c))
                }
                tail!($slow, tag.rebox(acc))
            }

            #[cold]
            #[inline(never)]
            fn $slow(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                let c = if $swap { generic_compare($le, acc, lhs) } else { generic_compare($le, lhs, acc) };
                next!(any, Op::$op.size(), Value::from_bool(c))
            }

            fn $any_jt(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if !tag.both_int32(lhs, acc) {
                    tail!($slow, acc)
                }
                if rel_i32($le, $swap, lhs.as_i32_unchecked(), acc.as_i32_unchecked()) {
                    next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::TRUE)
                }
                next!(any, 2 + Op::JumpIfTrue.size(), Value::FALSE)
            }

            fn $any_jf(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if !tag.both_int32(lhs, acc) {
                    tail!($slow, acc)
                }
                if !rel_i32($le, $swap, lhs.as_i32_unchecked(), acc.as_i32_unchecked()) {
                    next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::FALSE)
                }
                next!(any, 2 + Op::JumpIfFalse.size(), Value::TRUE)
            }

            fn $i32_jt(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if !tag.is_int32(lhs) {
                    tail!($slow, tag.rebox(acc))
                }
                if rel_i32($le, $swap, lhs.as_i32_unchecked(), raw_i32(acc)) {
                    next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::TRUE)
                }
                next!(any, 2 + Op::JumpIfTrue.size(), Value::FALSE)
            }

            fn $i32_jf(pc, acc, regs, tag) {
                let lhs = *reg(pc, 1, regs);
                if !tag.is_int32(lhs) {
                    tail!($slow, tag.rebox(acc))
                }
                if !rel_i32($le, $swap, lhs.as_i32_unchecked(), raw_i32(acc)) {
                    next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::FALSE)
                }
                next!(any, 2 + Op::JumpIfFalse.size(), Value::TRUE)
            }
        }
    };
}

/// `a === b` without a runtime call, if the bits decide: equal bits (but
/// a NaN's), or different ones where neither is a double and not both are
/// cells (two cells may be equal strings).
#[inline(always)]
fn strict_fast(tag: NumberTag, a: Value, b: Value) -> Option<bool> {
    if tag.is_double(a) || tag.is_double(b) {
        return None;
    }
    if a == b {
        return Some(true);
    }
    // Two cells are equal only as strings.
    if a.is_cell() && b.is_cell() && is_string(a) && is_string(b) {
        return None;
    }
    Some(false)
}

/// `l < r` or `l <= r`, or with `swap` `r < l` / `r <= l`.
#[inline(always)]
fn rel_i32(le: bool, swap: bool, l: i32, r: i32) -> bool {
    let (a, b) = if swap { (r, l) } else { (l, r) };
    if le { a <= b } else { a < b }
}

relational!(
    TestGreaterThan,
    TestGreaterThanJumpIfTrue,
    TestGreaterThanJumpIfFalse,
    false,
    true,
    op_test_gt,
    i32_test_gt,
    slow_test_gt,
    op_test_gt_jt,
    op_test_gt_jf,
    i32_test_gt_jt,
    i32_test_gt_jf
);
relational!(
    TestLessThanOrEqual,
    TestLessThanOrEqualJumpIfTrue,
    TestLessThanOrEqualJumpIfFalse,
    true,
    false,
    op_test_le,
    i32_test_le,
    slow_test_le,
    op_test_le_jt,
    op_test_le_jf,
    i32_test_le_jt,
    i32_test_le_jf
);
relational!(
    TestGreaterThanOrEqual,
    TestGreaterThanOrEqualJumpIfTrue,
    TestGreaterThanOrEqualJumpIfFalse,
    true,
    true,
    op_test_ge,
    i32_test_ge,
    slow_test_ge,
    op_test_ge_jt,
    op_test_ge_jf,
    i32_test_ge_jt,
    i32_test_ge_jf
);

/// A test of `acc` alone (`TestUndefined`, …), plain and fused with the
/// jump after it. It ignores the state: both tables use these.
macro_rules! acc_test {
    ($op:ident, $test:ident, $jt:ident, $jf:ident, |$v:ident| $c:expr) => {
        handlers! {
            fn $test(pc, acc, regs, tag) {
                let $v = acc;
                next!(any, Op::$op.size(), Value::from_bool($c))
            }

            fn $jt(pc, acc, regs, tag) {
                let $v = acc;
                if $c {
                    next!(any, 1 + imm32(pc, 1 + 1) as isize, Value::TRUE)
                }
                next!(any, 1 + Op::JumpIfTrue.size(), Value::FALSE)
            }

            fn $jf(pc, acc, regs, tag) {
                let $v = acc;
                if !$c {
                    next!(any, 1 + imm32(pc, 1 + 1) as isize, Value::FALSE)
                }
                next!(any, 1 + Op::JumpIfFalse.size(), Value::TRUE)
            }
        }
    };
}

// In the I32 state `acc` is an int32, which is none of these: they read
// it unboxed or boxed alike, as an int32's bits are neither 0x2 nor 0xA.
acc_test!(
    TestUndefined,
    op_test_undefined,
    op_test_undefined_jt,
    op_test_undefined_jf,
    |v| v == Value::UNDEFINED
);
acc_test!(
    TestNull,
    op_test_null,
    op_test_null_jt,
    op_test_null_jf,
    |v| v == Value::NULL
);
acc_test!(
    TestNullish,
    op_test_nullish,
    op_test_nullish_jt,
    op_test_nullish_jf,
    |v| v.to_bits() & !Value::UNDEFINED_TAG == Value::NULL.to_bits()
);

handlers! {
    // Without a double on either side, strict equality is equality of the
    // bits; a double needs a numeric compare (NaN, ±0, an int32 equal to
    // it).
    fn op_test_strict_equal(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        if let Some(c) = strict_fast(tag, lhs, acc) {
            next!(any, Op::TestStrictEqual.size(), Value::from_bool(c))
        }
        tail!(slow_test_strict_equal, acc)
    }

    #[cold]
    #[inline(never)]
    fn slow_test_strict_equal(pc, acc, regs, tag) {
        let c = strict_equals(*reg(pc, 1, regs), acc);
        next!(any, Op::TestStrictEqual.size(), Value::from_bool(c))
    }

    fn op_test_strict_equal_jt(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        let Some(c) = strict_fast(tag, lhs, acc) else {
            tail!(slow_test_strict_equal, acc)
        };
        if c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::TRUE)
        }
        next!(any, 2 + Op::JumpIfTrue.size(), Value::FALSE)
    }

    fn op_test_strict_equal_jf(pc, acc, regs, tag) {
        let lhs = *reg(pc, 1, regs);
        let Some(c) = strict_fast(tag, lhs, acc) else {
            tail!(slow_test_strict_equal, acc)
        };
        if !c {
            next!(any, 2 + imm32(pc, 2 + 1) as isize, Value::FALSE)
        }
        next!(any, 2 + Op::JumpIfFalse.size(), Value::TRUE)
    }
}

/// A compare-and-jump `acc OP x` (`x` a register or the immediate; `$le`
/// picks `<=`, `$swap` compares `x OP acc`: `>` is `x < acc`), plain and
/// loop forms, in both states.
macro_rules! compare_jump {
    ($op:ident, $loop_op:ident, $le:literal, $swap:literal, $smi:tt,
     $any:ident, $i32:ident, $slow:ident, $any_loop:ident, $i32_loop:ident, $slow_loop:ident) => {
        handlers! {
            fn $any(pc, acc, regs, tag) {
                let x = compare_jump!(@operand $smi, pc, regs);
                if !tag.both_int32(acc, x) {
                    tail!($slow, acc)
                }
                let c = rel_i32($le, $swap, acc.as_i32_unchecked(), x.as_i32_unchecked());
                if c {
                    if ACC_I32 { next!(i32, compare_jump!(@off $smi, pc), acc) } else { next!(any, compare_jump!(@off $smi, pc), acc) }
                }
                if ACC_I32 { next!(i32, Op::$op.size(), acc) } else { next!(any, Op::$op.size(), acc) }
            }

            fn $i32(pc, acc, regs, tag) {
                let x = compare_jump!(@operand $smi, pc, regs);
                if !tag.is_int32(x) {
                    tail!($slow, tag.rebox(acc))
                }
                if rel_i32($le, $swap, raw_i32(acc), x.as_i32_unchecked()) {
                    next!(i32, compare_jump!(@off $smi, pc), acc)
                }
                next!(i32, Op::$op.size(), acc)
            }

            #[cold]
            #[inline(never)]
            fn $slow(pc, acc, regs, tag) {
                let x = compare_jump!(@operand $smi, pc, regs);
                let c = if $swap { generic_compare($le, x, acc) } else { generic_compare($le, acc, x) };
                if c {
                    next!(any, compare_jump!(@off $smi, pc), acc)
                }
                next!(any, Op::$op.size(), acc)
            }

            fn $any_loop(pc, acc, regs, tag) {
                let x = compare_jump!(@operand $smi, pc, regs);
                if !tag.both_int32(acc, x) {
                    tail!($slow_loop, acc)
                }
                if rel_i32($le, $swap, acc.as_i32_unchecked(), x.as_i32_unchecked()) {
                    let off = compare_jump!(@off $smi, pc);
                    back_edge!(regs, pc.offset(off as isize), acc);
                    if ACC_I32 { next!(i32, off, acc) } else { next!(any, off, acc) }
                }
                if ACC_I32 { next!(i32, Op::$op.size(), acc) } else { next!(any, Op::$op.size(), acc) }
            }

            fn $i32_loop(pc, acc, regs, tag) {
                let x = compare_jump!(@operand $smi, pc, regs);
                if !tag.is_int32(x) {
                    tail!($slow_loop, tag.rebox(acc))
                }
                if rel_i32($le, $swap, raw_i32(acc), x.as_i32_unchecked()) {
                    let off = compare_jump!(@off $smi, pc);
                    back_edge!(regs, pc.offset(off as isize), tag.rebox(acc));
                    next!(i32, off, acc)
                }
                next!(i32, Op::$op.size(), acc)
            }

            #[cold]
            #[inline(never)]
            fn $slow_loop(pc, acc, regs, tag) {
                let x = compare_jump!(@operand $smi, pc, regs);
                let c = if $swap { generic_compare($le, x, acc) } else { generic_compare($le, acc, x) };
                if c {
                    let off = compare_jump!(@off $smi, pc);
                    back_edge!(regs, pc.offset(off as isize), acc);
                    next!(any, off, acc)
                }
                next!(any, Op::$op.size(), acc)
            }
        }
    };
    (@operand reg, $pc:ident, $regs:ident) => { *reg($pc, 1, $regs) };
    (@operand smi, $pc:ident, $regs:ident) => { Value::from_i32(imm32($pc, 1)) };
    (@off reg, $pc:ident) => { imm32($pc, 2) };
    (@off smi, $pc:ident) => { imm32($pc, 5) };
}

handlers! {
    fn op_jump_if_strict_equal_smi(pc, acc, regs, tag) {
        let k = imm32(pc, 1);
        let c = if tag.is_int32(acc) {
            acc.as_i32_unchecked() == k
        } else {
            tag.is_double(acc) && tag.decode_f64(acc) == k as f64
        };
        if c {
            next!(any, imm32(pc, 5), acc)
        }
        next!(any, Op::JumpIfStrictEqualSmi.size(), acc)
    }

    fn i32_jump_if_strict_equal_smi(pc, acc, regs, tag) {
        if raw_i32(acc) == imm32(pc, 1) {
            next!(i32, imm32(pc, 5), acc)
        }
        next!(i32, Op::JumpIfStrictEqualSmi.size(), acc)
    }
}

compare_jump!(
    JumpIfLessEqual,
    JumpIfLessEqualLoop,
    true,
    false,
    reg,
    op_jle,
    i32_jle,
    slow_jle,
    op_jle_loop,
    i32_jle_loop,
    slow_jle_loop
);
compare_jump!(
    JumpIfGreater,
    JumpIfGreaterLoop,
    false,
    true,
    reg,
    op_jgt,
    i32_jgt,
    slow_jgt,
    op_jgt_loop,
    i32_jgt_loop,
    slow_jgt_loop
);
compare_jump!(
    JumpIfGreaterEqual,
    JumpIfGreaterEqualLoop,
    true,
    true,
    reg,
    op_jge,
    i32_jge,
    slow_jge,
    op_jge_loop,
    i32_jge_loop,
    slow_jge_loop
);
compare_jump!(
    JumpIfLessEqualSmi,
    JumpIfLessEqualSmiLoop,
    true,
    false,
    smi,
    op_jle_smi,
    i32_jle_smi,
    slow_jle_smi,
    op_jle_smi_loop,
    i32_jle_smi_loop,
    slow_jle_smi_loop
);
compare_jump!(
    JumpIfGreaterSmi,
    JumpIfGreaterSmiLoop,
    false,
    true,
    smi,
    op_jgt_smi,
    i32_jgt_smi,
    slow_jgt_smi,
    op_jgt_smi_loop,
    i32_jgt_smi_loop,
    slow_jgt_smi_loop
);
compare_jump!(
    JumpIfGreaterEqualSmi,
    JumpIfGreaterEqualSmiLoop,
    true,
    true,
    smi,
    op_jge_smi,
    i32_jge_smi,
    slow_jge_smi,
    op_jge_smi_loop,
    i32_jge_smi_loop,
    slow_jge_smi_loop
);

#[inline(never)]
fn create_closure(regs: *mut Value, i: u16) -> Value {
    unsafe {
        let code = &*frame_code(regs);
        let callee = code.closures[i as usize];
        if (&(*callee).upvalues).is_empty() {
            return (*frame_vm(regs)).new_closure(callee);
        }
        // Upvalues of this frame's registers: closed when it ends.
        if (*callee).upvalues.iter().any(|&(local, _)| local) {
            *regs.sub(FLAGS) = Value::from_bits((*regs.sub(FLAGS)).to_bits() | CAPTURED);
        }
        let current: *const crate::object::Object = header(regs, FUNC);
        (*frame_vm(regs)).create_closure_in(code, i, regs, current)
    }
}

#[inline(never)]
fn create_object(regs: *mut Value, ic: u16, n: u32) -> Value {
    unsafe { (*frame_vm(regs)).create_object(&*frame_code(regs), ic, n) }
}

#[inline(never)]
fn new_array(regs: *mut Value) -> Value {
    unsafe { (*frame_vm(regs)).new_array() }
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
    if let Some(v) = concat_fast(a, b) {
        return v;
    }
    // Strings and int32s, the common case, straight into the result.
    if let (Some(p), Some(q)) = (Piece::of(a), Piece::of(b))
        && (p.is_string() || q.is_string())
    {
        return concat(p, q);
    }
    // ToPrimitive: an object is its string (no `valueOf` yet).
    let (a, b) = (to_primitive(a), to_primitive(b));
    if is_string(a) || is_string(b) {
        let mut units = to_string_units(a);
        units.extend(to_string_units(b));
        return unsafe { (*current_vm()).new_string(&units) };
    }
    Value::number(to_number(a) + to_number(b))
}

/// `a + b` for two strings, or a string and an int32 (`"v" + i`): the
/// string copied straight into the new string, a word at a time, and the
/// int32's digits written there. In Octane's DeltaBlue (2.65M a run) going
/// through [`Piece`] cost about 290 instructions each, a third of them
/// calls to `memmove`; reading the digits back a word at a time from where
/// they were written a unit at a time stalled each copy (no store
/// forwarding from narrower stores): 2% of DeltaBlue's cycles.
#[inline(always)]
fn concat_fast(a: Value, b: Value) -> Option<Value> {
    let vm = unsafe { &mut *current_vm() };
    // A word more than the units need: the last word copied may run past
    // the end.
    let words = |len: usize| len.div_ceil(4) + 1;
    match (string_units(a), string_units(b)) {
        (Some(x), Some(y)) => {
            let len = x.len() + y.len();
            Some(vm.new_string_raw(len, words(len), |out| unsafe {
                copy_words(out, x.as_ptr(), x.len());
                copy_words(out.add(x.len()), y.as_ptr(), y.len());
            }))
        }
        (Some(x), None) if b.is_int32() => {
            let i = b.as_i32_unchecked();
            let (n, len) = (int_len(i), x.len() + int_len(i));
            // The copy may run into the digits' units: they come after.
            Some(vm.new_string_raw(len, words(len), |out| unsafe {
                copy_words(out, x.as_ptr(), x.len());
                write_int(out.add(x.len()), i, n);
            }))
        }
        (None, Some(y)) if a.is_int32() => {
            let i = a.as_i32_unchecked();
            let (n, len) = (int_len(i), int_len(i) + y.len());
            Some(vm.new_string_raw(len, words(len), |out| unsafe {
                write_int(out, i, n);
                copy_words(out.add(n), y.as_ptr(), y.len());
            }))
        }
        _ => None,
    }
}

/// The units of `i`'s decimal form: its digits, and a sign.
#[inline(always)]
fn int_len(i: i32) -> usize {
    let n = i.unsigned_abs();
    let digits = match n {
        0..10 => 1,
        10..100 => 2,
        100..1_000 => 3,
        1_000..10_000 => 4,
        10_000..100_000 => 5,
        100_000..1_000_000 => 6,
        1_000_000..10_000_000 => 7,
        10_000_000..100_000_000 => 8,
        100_000_000..1_000_000_000 => 9,
        _ => 10,
    };
    digits + (i < 0) as usize
}

/// Write `i`'s decimal form, `len` units ([`int_len`]), to `to`.
///
/// # Safety
/// `to` must have room for `len` units.
#[inline(always)]
unsafe fn write_int(to: *mut u16, i: i32, len: usize) {
    let mut n = i.unsigned_abs();
    let mut at = len;
    loop {
        at -= 1;
        unsafe { *to.add(at) = b'0' as u16 + (n % 10) as u16 };
        n /= 10;
        if n == 0 {
            break;
        }
    }
    if i < 0 {
        unsafe { *to = b'-' as u16 };
    }
}

/// Copy `n` units from `from` to `to` four at a time, reading and writing
/// up to three past them: a string's units are padded to whole words.
///
/// # Safety
/// Both must have room for `n` rounded up to a multiple of four.
#[inline(always)]
unsafe fn copy_words(to: *mut u16, from: *const u16, n: usize) {
    let mut k = 0;
    while k < n {
        unsafe {
            let w = from.add(k).cast::<u64>().read_unaligned();
            to.add(k).cast::<u64>().write_unaligned(w);
        }
        k += 4;
    }
}

/// An operand of a concatenation that needs no conversion through a
/// `String`: a string's code units, or an int32's decimal digits.
enum Piece<'a> {
    Units(&'a [u16]),
    /// The digits are `digits[start..]`.
    Int {
        digits: [u8; 11],
        start: usize,
    },
}

impl Piece<'_> {
    fn of(v: Value) -> Option<Self> {
        if v.is_int32() {
            let mut digits = [0u8; 11];
            let i = v.as_i32_unchecked();
            let mut n = i.unsigned_abs();
            let mut at = digits.len();
            loop {
                at -= 1;
                digits[at] = b'0' + (n % 10) as u8;
                n /= 10;
                if n == 0 {
                    break;
                }
            }
            if i < 0 {
                at -= 1;
                digits[at] = b'-';
            }
            return Some(Piece::Int { digits, start: at });
        }
        string_units(v).map(Piece::Units)
    }

    fn is_string(&self) -> bool {
        matches!(self, Piece::Units(_))
    }

    fn len(&self) -> usize {
        match self {
            Piece::Units(u) => u.len(),
            Piece::Int { digits, start } => digits.len() - start,
        }
    }

    fn write(&self, out: &mut [u16]) {
        match self {
            Piece::Units(u) => out.copy_from_slice(u),
            Piece::Int { digits, start } => {
                for (o, &d) in out.iter_mut().zip(&digits[*start..]) {
                    *o = d as u16;
                }
            }
        }
    }
}

/// `p + q` as a new string.
fn concat(p: Piece, q: Piece) -> Value {
    let (m, n) = (p.len(), q.len());
    unsafe {
        (*current_vm()).new_string_with(m + n, |out| {
            p.write(&mut out[..m]);
            q.write(&mut out[m..]);
        })
    }
}

/// `a + 1` where `a` is not a number.
#[cold]
#[inline(never)]
pub(crate) fn generic_inc(a: Value) -> Value {
    Value::number(to_number(a) + 1.0)
}

/// `a < b` (IsLessThan) where at least one side is not a number.
#[cold]
#[inline(never)]
pub(crate) fn generic_less_than(a: Value, b: Value) -> bool {
    generic_compare(false, a, b)
}

/// ECMAScript ToBoolean.
pub(crate) fn to_boolean(v: Value) -> bool {
    if let Some(d) = v.as_number() {
        d != 0.0 && !d.is_nan()
    } else if let Some(u) = string_units(v) {
        !u.is_empty()
    } else {
        // Objects are truthy; there are no BigInts yet.
        v == Value::TRUE || v.is_cell()
    }
}

/// ECMAScript ToInt32 of a number.
/// ToInt32 of any value (ToNumber first).
#[inline]
fn int32_of(v: Value) -> i32 {
    if v.is_int32() {
        v.as_i32_unchecked()
    } else {
        to_int32(to_number(v))
    }
}

pub(crate) fn to_int32(d: f64) -> i32 {
    if !d.is_finite() {
        return 0;
    }
    d.trunc().rem_euclid(4294967296.0) as u32 as i32
}

/// `a == b` (IsLooselyEqual) for the values that exist so far.
pub(crate) fn loose_equals(a: Value, b: Value) -> Result<bool, Error> {
    if let (Some(x), Some(y)) = (a.as_number(), b.as_number()) {
        return Ok(x == y);
    }
    if a == b {
        return Ok(true);
    }
    let nullish = |v: Value| v.is_null() || v.is_undefined();
    if nullish(a) || nullish(b) {
        return Ok(nullish(a) && nullish(b));
    }
    if a.is_bool() {
        return loose_equals(Value::from_i32((a == Value::TRUE) as i32), b);
    }
    if b.is_bool() {
        return loose_equals(a, Value::from_i32((b == Value::TRUE) as i32));
    }
    // Two objects that aren't both strings: the same one only.
    if a.is_cell() && b.is_cell() && !(is_string(a) && is_string(b)) {
        return Ok(false);
    }
    match (string_units(a), string_units(b)) {
        (Some(p), Some(q)) => return Ok(p == q),
        // A string and a number: as numbers.
        (Some(_), None) if b.is_number() => return Ok(to_number(a) == to_number(b)),
        (None, Some(_)) if a.is_number() => return Ok(to_number(a) == to_number(b)),
        _ => {}
    }
    if a.is_cell() && b.is_cell() {
        return Ok(false);
    }
    // An object and a primitive: the object's primitive (its string).
    if a.is_cell() {
        return loose_equals(to_primitive(a), b);
    }
    loose_equals(a, to_primitive(b))
}

/// The VM running (stencils' runtime calls and slow paths reach it this
/// way to allocate strings).
pub(crate) fn current_vm() -> *mut Vm {
    CURRENT_VM.with(|v| v.get())
}

thread_local! {
    pub(crate) static CURRENT_VM: std::cell::Cell<*mut Vm> = const { std::cell::Cell::new(std::ptr::null_mut()) };
}

fn is_string(v: Value) -> bool {
    string_units(v).is_some()
}

/// ToPrimitive (hint "default", as "number"): an object's `valueOf`, else
/// its `toString`, whichever gives a primitive first.
pub(crate) fn to_primitive(v: Value) -> Value {
    to_primitive_hint(v, false)
}

/// ToPrimitive with hint "string" (`string`: `toString` first) or
/// "number". Calls native methods only: a bytecode one (natives can't call
/// those yet) gets the default's result (`Object.prototype.toString`, or
/// an array's `join`).
fn to_primitive_hint(v: Value, string: bool) -> Value {
    if !v.is_cell() || is_string(v) {
        return v;
    }
    unsafe {
        let vm = &mut *current_vm();
        let order = if string {
            [vm.a_to_string, vm.a_value_of]
        } else {
            [vm.a_value_of, vm.a_to_string]
        };
        for name in order {
            let Ok(m) = vm.get(v, name) else { continue };
            if !m.is_cell() || (*as_object(m)).class != Class::Native {
                continue;
            }
            let native = (*(*as_object(m)).code).native.unwrap();
            if let Ok(r) = native(vm, v, &[])
                && (!r.is_cell() || is_string(r))
            {
                return r;
            }
        }
        let units = default_string(v);
        vm.new_string(&units)
    }
}

/// ToString, as UTF-16 code units.
pub(crate) fn to_string_units(v: Value) -> Vec<u16> {
    if let Some(u) = string_units(v) {
        return u.to_vec();
    }
    if v.is_cell() {
        return to_string_units(to_primitive_hint(v, true));
    }
    crate::vm::display(v).encode_utf16().collect()
}

/// An object's string without its methods: an array's elements joined by
/// commas, else `[object Object]`.
fn default_string(v: Value) -> Vec<u16> {
    let s = if v.is_cell() {
        let o = as_object(v);
        // An array: its elements' strings, joined by commas (holes,
        // undefined and null as empty strings).
        if unsafe { (*o).class } == Class::Array {
            let n = unsafe { (*o).elems_len } as usize;
            let mut out = Vec::new();
            for i in 0..n {
                if i > 0 {
                    out.push(b',' as u16);
                }
                let e = unsafe { *(*o).elems.add(i) };
                if e != Value::EMPTY && !e.is_undefined() && !e.is_null() {
                    out.extend(to_string_units(e));
                }
            }
            return out;
        }
        "[object Object]".to_string()
    } else {
        crate::vm::display(v)
    };
    s.encode_utf16().collect()
}

/// ToNumber: a string is parsed (StringToNumber), an object goes through
/// its primitive.
pub(crate) fn to_number(v: Value) -> f64 {
    if !v.is_cell() {
        return v.to_number();
    }
    if let Some(t) = crate::vm::date_value(v) {
        return t;
    }
    match string_units(v) {
        Some(u) => string_to_number(&String::from_utf16_lossy(u)),
        None => to_number(to_primitive(v)),
    }
}

/// StringToNumber: whitespace-trimmed decimal (with sign, fraction and
/// exponent), `0x`/`0o`/`0b` integers, or `Infinity`; empty is 0, anything
/// else NaN.
pub(crate) fn string_to_number(s: &str) -> f64 {
    let t = s.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    if t.is_empty() {
        return 0.0;
    }
    let radix = match t.get(..2) {
        Some("0x" | "0X") => 16,
        Some("0o" | "0O") => 8,
        Some("0b" | "0B") => 2,
        _ => 10,
    };
    if radix != 10 {
        let digits = &t[2..];
        if digits.is_empty() || !digits.chars().all(|c| c.is_digit(radix)) {
            return f64::NAN;
        }
        return digits.chars().fold(0.0, |acc, c| {
            acc * radix as f64 + c.to_digit(radix).unwrap() as f64
        });
    }
    let unsigned = t.strip_prefix(['+', '-']).unwrap_or(t);
    if unsigned == "Infinity" {
        return if t.starts_with('-') {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        };
    }
    // A StrDecimalLiteral: digits, an optional fraction, an exponent.
    let b = unsigned.as_bytes();
    let mut i = 0;
    let digits = |i: &mut usize| {
        let start = *i;
        while *i < b.len() && b[*i].is_ascii_digit() {
            *i += 1;
        }
        *i - start
    };
    let mut n = digits(&mut i);
    if i < b.len() && b[i] == b'.' {
        i += 1;
        n += digits(&mut i);
    }
    if n == 0 {
        return f64::NAN;
    }
    if i < b.len() && (b[i] | 0x20) == b'e' {
        i += 1;
        if i < b.len() && matches!(b[i], b'+' | b'-') {
            i += 1;
        }
        if digits(&mut i) == 0 {
            return f64::NAN;
        }
    }
    if i != b.len() {
        return f64::NAN;
    }
    t.parse().unwrap_or(f64::NAN)
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
    t[Op::Throw as usize] = op_throw;
    t[Op::TypeOf as usize] = op_type_of;
    t[Op::ToNumber as usize] = op_to_number;
    t[Op::Negate as usize] = op_negate;
    t[Op::BitNot as usize] = op_bit_not;
    t[Op::Mod as usize] = op_mod;
    t[Op::BitAnd as usize] = op_bit_and;
    t[Op::BitOr as usize] = op_bit_or;
    t[Op::BitXor as usize] = op_bit_xor;
    t[Op::ShiftLeft as usize] = op_shift_left;
    t[Op::ShiftRight as usize] = op_shift_right;
    t[Op::ShiftRightLogical as usize] = op_shift_right_logical;
    t[Op::LdaUpvalue as usize] = op_lda_upvalue;
    t[Op::StaUpvalue as usize] = op_sta_upvalue;
    t[Op::LdaGlobalInsideTypeof as usize] = op_lda_global_inside_typeof;
    t[Op::LdaUndefined as usize] = op_lda_undefined;
    t[Op::LdaNull as usize] = op_lda_null;
    t[Op::LdaTrue as usize] = op_lda_true;
    t[Op::LdaFalse as usize] = op_lda_false;
    t[Op::Mov as usize] = op_mov;
    t[Op::ModSmi as usize] = op_mod_smi;
    t[Op::BitAndSmi as usize] = op_bit_and_smi;
    t[Op::BitOrSmi as usize] = op_bit_or_smi;
    t[Op::TestLessThan as usize] = op_test_less_than;
    t[Op::TestEqual as usize] = op_test_equal;
    t[Op::TestEqualSmi as usize] = op_test_equal_smi;
    t[Op::JumpIfTrue as usize] = op_jump_if_true;
    t[Op::JumpIfFalse as usize] = op_jump_if_false;
    t[Op::JumpIfToBooleanTrue as usize] = op_jump_if_to_boolean_true;
    t[Op::JumpIfToBooleanFalse as usize] = op_jump_if_to_boolean_false;
    t[Op::LdaGlobal as usize] = op_lda_global;
    t[Op::CreateArguments as usize] = op_create_arguments;
    t[Op::TestInstanceOf as usize] = op_test_instance_of;
    t[Op::TestIn as usize] = op_test_in;
    t[Op::DeleteProperty as usize] = op_delete_property;
    t[Op::DeleteGlobal as usize] = op_delete_global;
    t[Op::LdaCallee as usize] = op_lda_callee;
    t[Op::DefineAccessor as usize] = op_define_accessor;
    t[Op::CreateRegExp as usize] = op_create_regexp;
    t[Op::StaGlobal as usize] = op_sta_global;
    t[Op::GetNamed as usize] = op_get_named;
    t[Op::SetNamed as usize] = op_set_named;
    t[Op::GetKeyed as usize] = op_get_keyed;
    t[Op::SetKeyed as usize] = op_set_keyed;
    t[Op::CreateClosure as usize] = op_create_closure;
    t[Op::CreateArray as usize] = op_create_array;
    t[Op::LdaConstant as usize] = op_lda_constant;
    t[Op::CreateObject as usize] = op_create_object;
    t[Op::ForInKeys as usize] = op_for_in_keys;
    t[Op::Call as usize] = op_call;
    t[Op::CallUndefined as usize] = op_call_undefined;
    t[Op::Construct as usize] = op_construct;
    t[Op::TestLessThanJumpIfTrue as usize] = op_test_less_than_jump_if_true;
    t[Op::TestEqualJumpIfTrue as usize] = op_test_equal_jump_if_true;
    t[Op::TestEqualSmiJumpIfTrue as usize] = op_test_equal_smi_jump_if_true;
    t[Op::TestLessThanJumpIfFalse as usize] = op_test_less_than_jump_if_false;
    t[Op::TestEqualJumpIfFalse as usize] = op_test_equal_jump_if_false;
    t[Op::TestEqualSmiJumpIfFalse as usize] = op_test_equal_smi_jump_if_false;
    t[Op::Sub as usize] = op_sub;
    t[Op::SubSmi as usize] = op_sub_smi;
    t[Op::Mul as usize] = op_mul;
    t[Op::MulSmi as usize] = op_mul_smi;
    t[Op::Div as usize] = op_div;
    t[Op::DivSmi as usize] = op_div_smi;
    t[Op::ShiftLeftSmi as usize] = op_shl_smi;
    t[Op::ShiftRightSmi as usize] = op_sar_smi;
    t[Op::ShiftRightLogicalSmi as usize] = op_shr_smi;
    t[Op::TestGreaterThan as usize] = op_test_gt;
    t[Op::TestLessThanOrEqual as usize] = op_test_le;
    t[Op::TestGreaterThanOrEqual as usize] = op_test_ge;
    t[Op::TestStrictEqual as usize] = op_test_strict_equal;
    t[Op::TestUndefined as usize] = op_test_undefined;
    t[Op::TestNull as usize] = op_test_null;
    t[Op::TestNullish as usize] = op_test_nullish;
    t[Op::JumpIfLessEqual as usize] = op_jle;
    t[Op::JumpIfGreater as usize] = op_jgt;
    t[Op::JumpIfGreaterEqual as usize] = op_jge;
    t[Op::JumpIfLessEqualSmi as usize] = op_jle_smi;
    t[Op::JumpIfGreaterSmi as usize] = op_jgt_smi;
    t[Op::JumpIfGreaterEqualSmi as usize] = op_jge_smi;
    t[Op::JumpIfLessEqualLoop as usize] = op_jle_loop;
    t[Op::JumpIfGreaterLoop as usize] = op_jgt_loop;
    t[Op::JumpIfGreaterEqualLoop as usize] = op_jge_loop;
    t[Op::JumpIfLessEqualSmiLoop as usize] = op_jle_smi_loop;
    t[Op::JumpIfGreaterSmiLoop as usize] = op_jgt_smi_loop;
    t[Op::JumpIfGreaterEqualSmiLoop as usize] = op_jge_smi_loop;
    t[Op::JumpIfStrictEqualSmi as usize] = op_jump_if_strict_equal_smi;
    t[Op::TestGreaterThanJumpIfTrue as usize] = op_test_gt_jt;
    t[Op::TestGreaterThanJumpIfFalse as usize] = op_test_gt_jf;
    t[Op::TestLessThanOrEqualJumpIfTrue as usize] = op_test_le_jt;
    t[Op::TestLessThanOrEqualJumpIfFalse as usize] = op_test_le_jf;
    t[Op::TestGreaterThanOrEqualJumpIfTrue as usize] = op_test_ge_jt;
    t[Op::TestGreaterThanOrEqualJumpIfFalse as usize] = op_test_ge_jf;
    t[Op::TestStrictEqualJumpIfTrue as usize] = op_test_strict_equal_jt;
    t[Op::TestStrictEqualJumpIfFalse as usize] = op_test_strict_equal_jf;
    t[Op::TestUndefinedJumpIfTrue as usize] = op_test_undefined_jt;
    t[Op::TestUndefinedJumpIfFalse as usize] = op_test_undefined_jf;
    t[Op::TestNullJumpIfTrue as usize] = op_test_null_jt;
    t[Op::TestNullJumpIfFalse as usize] = op_test_null_jf;
    t[Op::TestNullishJumpIfTrue as usize] = op_test_nullish_jt;
    t[Op::TestNullishJumpIfFalse as usize] = op_test_nullish_jf;
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
    // Handlers that ignore the incoming `acc` serve both states.
    t[Op::LdaUndefined as usize] = op_lda_undefined;
    t[Op::LdaNull as usize] = op_lda_null;
    t[Op::LdaTrue as usize] = op_lda_true;
    t[Op::LdaFalse as usize] = op_lda_false;
    t[Op::LdaGlobal as usize] = op_lda_global;
    t[Op::GetNamed as usize] = op_get_named;
    t[Op::CreateClosure as usize] = op_create_closure;
    t[Op::CreateArray as usize] = op_create_array;
    t[Op::LdaConstant as usize] = op_lda_constant;
    t[Op::CreateObject as usize] = op_create_object;
    t[Op::Call as usize] = op_call;
    t[Op::CallUndefined as usize] = op_call_undefined;
    t[Op::Construct as usize] = op_construct;
    t[Op::Mov as usize] = i32_mov;
    t[Op::ModSmi as usize] = i32_mod_smi;
    t[Op::BitAndSmi as usize] = i32_bit_and_smi;
    t[Op::BitOrSmi as usize] = i32_bit_or_smi;
    t[Op::TestLessThan as usize] = i32_test_less_than;
    t[Op::TestEqualSmi as usize] = i32_test_equal_smi;
    t[Op::JumpIfToBooleanTrue as usize] = i32_jump_if_to_boolean_true;
    t[Op::JumpIfToBooleanFalse as usize] = i32_jump_if_to_boolean_false;
    t[Op::GetKeyed as usize] = i32_get_keyed;
    t[Op::SetNamed as usize] = i32_set_named;
    t[Op::TestLessThanJumpIfTrue as usize] = i32_test_less_than_jump_if_true;
    t[Op::TestEqualSmiJumpIfTrue as usize] = i32_test_equal_smi_jump_if_true;
    t[Op::TestLessThanJumpIfFalse as usize] = i32_test_less_than_jump_if_false;
    t[Op::TestEqualSmiJumpIfFalse as usize] = i32_test_equal_smi_jump_if_false;
    t[Op::Sub as usize] = i32_sub;
    t[Op::SubSmi as usize] = i32_sub_smi;
    t[Op::Mul as usize] = i32_mul;
    t[Op::MulSmi as usize] = i32_mul_smi;
    t[Op::Div as usize] = i32_div;
    t[Op::DivSmi as usize] = i32_div_smi;
    t[Op::ShiftLeftSmi as usize] = i32_shl_smi;
    t[Op::ShiftRightSmi as usize] = i32_sar_smi;
    t[Op::ShiftRightLogicalSmi as usize] = i32_shr_smi;
    t[Op::TestGreaterThan as usize] = i32_test_gt;
    t[Op::TestLessThanOrEqual as usize] = i32_test_le;
    t[Op::TestGreaterThanOrEqual as usize] = i32_test_ge;
    t[Op::TestUndefined as usize] = op_test_undefined;
    t[Op::TestNull as usize] = op_test_null;
    t[Op::TestNullish as usize] = op_test_nullish;
    t[Op::JumpIfLessEqual as usize] = i32_jle;
    t[Op::JumpIfGreater as usize] = i32_jgt;
    t[Op::JumpIfGreaterEqual as usize] = i32_jge;
    t[Op::JumpIfLessEqualSmi as usize] = i32_jle_smi;
    t[Op::JumpIfGreaterSmi as usize] = i32_jgt_smi;
    t[Op::JumpIfGreaterEqualSmi as usize] = i32_jge_smi;
    t[Op::JumpIfLessEqualLoop as usize] = i32_jle_loop;
    t[Op::JumpIfGreaterLoop as usize] = i32_jgt_loop;
    t[Op::JumpIfGreaterEqualLoop as usize] = i32_jge_loop;
    t[Op::JumpIfLessEqualSmiLoop as usize] = i32_jle_smi_loop;
    t[Op::JumpIfGreaterSmiLoop as usize] = i32_jgt_smi_loop;
    t[Op::JumpIfGreaterEqualSmiLoop as usize] = i32_jge_smi_loop;
    t[Op::JumpIfStrictEqualSmi as usize] = i32_jump_if_strict_equal_smi;
    t[Op::TestGreaterThanJumpIfTrue as usize] = i32_test_gt_jt;
    t[Op::TestGreaterThanJumpIfFalse as usize] = i32_test_gt_jf;
    t[Op::TestLessThanOrEqualJumpIfTrue as usize] = i32_test_le_jt;
    t[Op::TestLessThanOrEqualJumpIfFalse as usize] = i32_test_le_jf;
    t[Op::TestGreaterThanOrEqualJumpIfTrue as usize] = i32_test_ge_jt;
    t[Op::TestGreaterThanOrEqualJumpIfFalse as usize] = i32_test_ge_jf;
    t[Op::TestUndefinedJumpIfTrue as usize] = op_test_undefined_jt;
    t[Op::TestUndefinedJumpIfFalse as usize] = op_test_undefined_jf;
    t[Op::TestNullJumpIfTrue as usize] = op_test_null_jt;
    t[Op::TestNullJumpIfFalse as usize] = op_test_null_jf;
    t[Op::TestNullishJumpIfTrue as usize] = op_test_nullish_jt;
    t[Op::TestNullishJumpIfFalse as usize] = op_test_nullish_jf;
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
        // A VM frame: tell `run_script` which one.
        let vm = frame_vm(regs);
        if !vm.is_null() {
            (*vm).osr_regs = regs;
        }
    }
    Value::EMPTY
}

/// Frame header slots, in front of the register file (see the module
/// docs). `regs[-BUDGET]` is the back-edge budget (an `i64`);
/// `regs[-OSR_PC]` and `regs[-OSR_ACC]` hold the target and accumulator
/// when the interpreter leaves to tier up.
const BUDGET: usize = 1;
const OSR_PC: usize = 2;
const OSR_ACC: usize = 3;
const CODE: usize = 4;
const ICS: usize = 5;
const VM: usize = 6;
const RET_PC: usize = 7;
const CALLER: usize = 8;
pub(crate) const FLAGS: usize = 9;
/// The function object called (for its upvalues; set by the interpreter's
/// calls, not by compiled code, which runs no function that has any).
const FUNC: usize = 10;
pub const HEADER: usize = 10;

/// `regs[-FLAGS]` of a frame entered by `Construct`.
pub(crate) const CONSTRUCT: u64 = 1;
/// `regs[-FLAGS]` of a frame whose registers open upvalues point to: its
/// return (or an unwind past it) closes them.
const CAPTURED: u64 = 2;

/// A pointer kept in a frame header slot.
#[inline(always)]
unsafe fn header<T>(regs: *mut Value, slot: usize) -> *mut T {
    unsafe { (*regs.sub(slot)).to_bits() as *mut T }
}

#[inline(always)]
unsafe fn set_header<T>(regs: *mut Value, slot: usize, p: *const T) {
    unsafe { *regs.sub(slot) = Value::from_bits(p as u64) }
}

#[inline(always)]
unsafe fn frame_vm(regs: *mut Value) -> *mut Vm {
    unsafe { header(regs, VM) }
}

#[inline(always)]
unsafe fn frame_code(regs: *mut Value) -> *const Code {
    unsafe { header(regs, CODE) }
}

/// The inline-cache slot named by the `u16` operand at `pc + off`: its
/// way 0, the others following.
#[inline(always)]
unsafe fn ic_at(regs: *mut Value, pc: *const u8, off: usize) -> *mut Ic {
    unsafe { header::<Ic>(regs, ICS).add(u16_at(pc, off) as usize * IC_WAYS) }
}

/// Record the object a call at `pc` calls in its inline cache, as the fast
/// paths do for bytecode functions: a cache left empty then means the call
/// never ran (the JIT leaves such calls to the interpreter).
#[inline(always)]
/// Call `f` with `this` and `args` from the runtime (a native, a getter or
/// a setter): a native directly; a bytecode function in a new frame at the
/// VM stack's free top, which the runtime code that may get here records
/// (`Vm::native_top`, see [`with_top`]), run to completion.
pub(crate) fn call_function(vm: &mut Vm, f: Value, this: Value, args: &[Value]) -> crate::vm::Result<Value> {
    unsafe {
        if !f.is_cell() || !matches!((*as_object(f)).class, Class::Function | Class::Native) {
            return Err(Error::type_error(format!("{} is not a function", crate::vm::display(f))));
        }
        if (*as_object(f)).class == Class::Native {
            return vm.call_other(f, this, args);
        }
        let top = vm.native_top;
        if top.is_null() {
            return Err(Error::unsupported("calling a function from here (compiled code)"));
        }
        let fo = as_object(f);
        let callee = (*fo).code;
        let new = top.add(HEADER);
        let n = (*callee).nregs as usize;
        if new.add(n) > vm.stack_end || native_stack_low(vm) {
            return Err(Error::range_error("Maximum call stack size exceeded"));
        }
        write_frame_header(vm, new, callee, std::ptr::null(), std::ptr::null_mut(), 0);
        set_header(new, FUNC, fo);
        *new = sloppy_this(vm, this);
        let k = args.len().min((*callee).nparams as usize);
        for i in 0..k {
            *new.add(1 + i) = args[i];
        }
        for i in 1 + k..n {
            *new.add(i) = Value::UNDEFINED;
        }
        vm.pending_args = (args.as_ptr(), args.len());
        let saved = vm.native_top;
        vm.native_top = std::ptr::null_mut();
        let v = run_new_frame(vm, callee, new, 0);
        vm.native_top = saved;
        if v == Value::EMPTY {
            return Err(vm.error.take().unwrap_or_else(|| Error::unsupported("a call from the runtime")));
        }
        Ok(v)
    }
}

/// Run the script `code` (`eval`'s, `Function`'s) in a new frame at the VM
/// stack's free top (as [`call_function`]), `this` the global object.
pub(crate) fn run_script_at(vm: &mut Vm, code: *const Code) -> crate::vm::Result<Value> {
    unsafe {
        let top = vm.native_top;
        if top.is_null() {
            return Err(Error::unsupported("eval from here (compiled code)"));
        }
        let new = top.add(HEADER);
        let n = (*code).nregs as usize;
        if new.add(n) > vm.stack_end || native_stack_low(vm) {
            return Err(Error::range_error("Maximum call stack size exceeded"));
        }
        write_frame_header(vm, new, code, std::ptr::null(), std::ptr::null_mut(), 0);
        set_header(new, FUNC, std::ptr::null_mut::<Object>());
        *new = object_value(vm.global);
        for i in 1..n {
            *new.add(i) = Value::UNDEFINED;
        }
        let saved = vm.native_top;
        vm.native_top = std::ptr::null_mut();
        let v = drive(vm, (*code).bytecode.as_ptr(), Value::UNDEFINED, new);
        vm.native_top = saved;
        if v == Value::EMPTY {
            return Err(vm.error.take().unwrap_or_else(|| Error::unsupported("eval")));
        }
        Ok(v)
    }
}

/// Run `f` with `Vm::native_top` at the free VM stack above the frame
/// `regs` (at its `top`, its registers' end, if given): what it calls
/// through [`call_function`] goes there.
#[inline(always)]
unsafe fn with_top<T>(regs: *mut Value, top: Option<*mut Value>, f: impl FnOnce(&mut Vm) -> T) -> T {
    unsafe {
        let vm = &mut *frame_vm(regs);
        let saved = vm.native_top;
        vm.native_top = top.unwrap_or_else(|| regs.add((*frame_code(regs)).nregs as usize));
        let r = f(vm);
        (*frame_vm(regs)).native_top = saved;
        r
    }
}

/// For a call of `f` with receiver `this`: if `f` is
/// `Function.prototype.apply` and `this` a bytecode function, `this`, which
/// is what the call calls.
pub(crate) unsafe fn unwrap_apply(vm: *const Vm, f: Value, this: Value) -> Option<*mut Object> {
    unsafe {
        let is = |v: Value, c: Class| v.is_cell() && (*as_object(v)).class == c;
        (is(f, Class::Native)
            && (*as_object(f)).code == (*vm).function_apply
            && is(this, Class::Function))
        .then(|| as_object(this))
    }
}

/// The `this` a bytecode function gets for receiver `this`: the global
/// object for `undefined` or `null` (sloppy mode; toi has no strict mode).
/// Calls without a receiver pass the global object themselves; this is for
/// `f.call(that, …)`.
pub(crate) unsafe fn sloppy_this(vm: *const Vm, this: Value) -> Value {
    if this == Value::UNDEFINED || this == Value::NULL {
        object_value(unsafe { (*vm).global })
    } else {
        this
    }
}

/// For a call of `f` with receiver `this`: if `f` is
/// `Function.prototype.call` and `this` a bytecode function, `this`, which
/// is what the call calls.
pub(crate) unsafe fn unwrap_call(vm: *const Vm, f: Value, this: Value) -> Option<*mut Object> {
    unsafe {
        let is = |v: Value, c: Class| v.is_cell() && (*as_object(v)).class == c;
        (is(f, Class::Native)
            && (*as_object(f)).code == (*vm).function_call
            && is(this, Class::Function))
        .then(|| as_object(this))
    }
}

/// Record that a call calls `f`: way 0's `holder` is the latest callee,
/// the other ways' the ones before it (distinct; the JIT inlines up to all
/// four). Only a change of callee costs more than a compare; another
/// closure of the same code (a function made anew at each call) just
/// replaces the one in way 0.
#[inline(always)]
unsafe fn note_callee(ic: *mut Ic, f: *mut Object) {
    unsafe {
        let old = (*ic).holder;
        if old != f {
            if !old.is_null() && (*old).code != (*f).code {
                keep_callee(ic, old, f);
            }
            (*ic).holder = f;
        }
    }
}

/// `note_callee`'s slow path: keep `old` in ways 1-3, unless there, and
/// take `new` out of them (it goes to way 0).
#[inline(never)]
unsafe fn keep_callee(ic: *mut Ic, old: *mut Object, new: *mut Object) {
    unsafe {
        for k in 1..IC_WAYS {
            let w = &mut *ic.add(k);
            if w.holder == new {
                w.holder = old;
                return;
            }
        }
        for k in 1..IC_WAYS {
            let w = &mut *ic.add(k);
            if w.holder.is_null() || w.holder == old {
                w.holder = old;
                return;
            }
        }
        // Megamorphic: the oldest goes.
        std::ptr::copy(ic.add(2), ic.add(1), IC_WAYS - 2);
        (*ic.add(IC_WAYS - 1)).holder = old;
    }
}

unsafe fn record_callee(pc: *const u8, regs: *mut Value, tag: NumberTag) {
    unsafe {
        let f = *reg(pc, 1, regs);
        if is_cell(tag, f) {
            note_callee(ic_at(regs, pc, 4), as_object(f));
        }
    }
}

/// Push a frame for `callee` past the caller's registers: header, `this`,
/// the first `argc` values at `args` as parameters, the rest `undefined`.
/// Null if the VM stack is full.
#[inline(always)]
#[allow(clippy::too_many_arguments)]
unsafe fn push_frame(
    regs: *mut Value,
    fo: *mut crate::object::Object,
    callee: *const Code,
    this: Value,
    args: *const Value,
    argc: usize,
    flags: u64,
    ret: *const u8,
) -> *mut Value {
    unsafe {
        let vm = frame_vm(regs);
        let new = regs.add((*frame_code(regs)).nregs as usize + HEADER);
        let n = (*callee).nregs as usize;
        if new.add(n) > (*vm).stack_end {
            return std::ptr::null_mut();
        }
        *new.sub(BUDGET) = Value::from_bits((*callee).loop_budget.get() as u64);
        set_header(new, CODE, callee);
        set_header(new, ICS, (*callee).ics.as_ptr());
        set_header(new, VM, vm);
        set_header(new, RET_PC, ret);
        set_header(new, CALLER, regs);
        *new.sub(FLAGS) = Value::from_bits(flags);
        set_header(new, FUNC, fo);
        *new = this;
        // Scalar loops: counts are small, and vectorized ones would bloat
        // the handler with prologues and remainders.
        let k = argc.min((*callee).nparams as usize);
        for i in 0..k {
            *new.add(1 + i) = *args.add(i);
            core::arch::asm!("", options(nomem, nostack, preserves_flags));
        }
        for i in 1 + k..n {
            *new.add(i) = Value::UNDEFINED;
            core::arch::asm!("", options(nomem, nostack, preserves_flags));
        }
        new
    }
}

#[cold]
#[inline(never)]
fn stack_overflow(regs: *mut Value) -> Value {
    throw(
        unsafe { frame_vm(regs) },
        Error::range_error("Maximum call stack size exceeded"),
    )
}

/// End the run with `e`: record it in the VM and return the `EMPTY` marker
/// straight to the host (every handler call is a tail call).
#[cold]
#[inline(never)]
fn throw(vm: *mut Vm, e: Error) -> Value {
    unsafe { (*vm).error = Some(e) };
    Value::EMPTY
}

/// `Return` from a `new` frame: the result is `acc` if it is an object,
/// else `this`, whose shape feeds the constructor's slack tracking.
#[inline(never)]
fn construct_result(regs: *mut Value, acc: Value) -> Value {
    if acc.is_cell() {
        return acc;
    }
    unsafe {
        let this = *regs;
        Vm::construct_done(&*frame_code(regs), as_object(this));
        this
    }
}

/// The receiver of a `new` on bytecode function `f`.
#[inline(never)]
fn construct_this(regs: *mut Value, f: *mut Object) -> Value {
    unsafe { (*frame_vm(regs)).construct_this(f) }
}

/// Whether `v` fits the field type a store's inline cache holds (encoded by
/// `FieldType::encode`; 0, the common case, checks nothing).
#[inline(always)]
fn fits_field(tag: NumberTag, want: u64, v: Value) -> bool {
    match want {
        0 => true,
        1 => tag.is_int32(v),
        2 => v.to_bits() & tag.to_bits() != 0,
        s => is_cell(tag, v) && unsafe { *(v.to_bits() as *const u64) } == s,
    }
}

/// Whether `v` is a cell (a heap pointer), testing against the tag
/// register rather than a 64-bit constant. Like JSC's, the test passes
/// `EMPTY`, which never reaches a register or the accumulator.
#[inline(always)]
fn is_cell(tag: NumberTag, v: Value) -> bool {
    v.to_bits() & tag.to_bits() == 0 && v.to_bits() & Value::OTHER_TAG == 0
}

/// A register file with the interpreter's frame header in front.
pub struct Frame {
    slots: Vec<Value>,
}

impl Frame {
    /// A frame holding a copy of `regs`.
    pub fn new(regs: &[Value]) -> Frame {
        // Null code and return pc: `Return` goes back to the host.
        let mut slots = vec![Value::EMPTY; HEADER];
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

/// Run `code`, a module's top-level code, in a fresh frame at the base of
/// `vm`'s stack, with the global object as `this`. Returns the completion
/// value.
///
/// # Safety
/// `code` must have been loaded into `vm` and be well formed, as for
/// [`run_frame`]; in addition, every inline-cache and closure operand must
/// be in range.
pub unsafe fn run_script(vm: &mut Vm, code: *const Code) -> Result<Value, Error> {
    unsafe {
        let n = (*code).nregs as usize;
        if HEADER + n > vm.stack.len() {
            return Err(Error::range_error("Maximum call stack size exceeded"));
        }
        let regs = vm.stack.as_mut_ptr().add(HEADER);
        *regs.sub(BUDGET) = Value::from_bits(vm.frame_budget as u64);
        set_header(regs, CODE, code);
        set_header(regs, ICS, (*code).ics.as_ptr());
        set_header(regs, VM, vm as *mut Vm);
        set_header(regs, RET_PC, std::ptr::null::<u8>());
        set_header(regs, CALLER, std::ptr::null::<Value>());
        *regs.sub(FLAGS) = Value::from_bits(0);
        *regs = crate::object::object_value(vm.global);
        for i in 1..n {
            *regs.add(i) = Value::UNDEFINED;
        }
        let v = drive(vm, (*code).bytecode.as_ptr(), Value::UNDEFINED, regs);
        match vm.error.take() {
            Some(e) => Err(e),
            None => Ok(v),
        }
    }
}

/// Run the interpreter from `pc` with `acc` over the VM frame `regs` until
/// that frame returns (with `RET_PC` null), handling tier-up requests: the
/// interpreter returns `EMPTY` (`osr_exit`), the frame's compiled code runs
/// from the loop header, and the interpreter resumes where the code exited,
/// maybe in a frame the code had inlined. The VM stack holds every frame,
/// so leaving the handler chain loses nothing. Returns `EMPTY` on an error,
/// left in `vm.error`.
pub(crate) unsafe fn drive(vm: &mut Vm, pc: *const u8, acc: Value, regs: *mut Value) -> Value {
    unsafe {
        let mut v = dispatch_at(pc, acc, regs);
        loop {
            if vm.error.is_some() || v != Value::EMPTY {
                return v;
            }
            let regs = std::mem::replace(&mut vm.osr_regs, std::ptr::null_mut());
            assert!(
                !regs.is_null(),
                "EMPTY without an error or a tier-up request"
            );
            let code = &*frame_code(regs);
            let start = code.bytecode.as_ptr();
            let target = (*regs.sub(OSR_PC)).to_bits() as usize - start as usize;
            let acc = *regs.sub(OSR_ACC);
            // Re-arm the budget even if this loop isn't compiled: another
            // loop of the frame may be.
            *regs.sub(BUDGET) = Value::from_bits(vm.frame_budget as u64);
            let resume = match crate::jit::vm_run(vm, code, regs, target, acc) {
                VmRun::NotCompiled | VmRun::Interpret => Resume {
                    regs,
                    pc: start.add(target),
                    acc,
                },
                VmRun::Exit(r) => r,
                VmRun::Returned(v) => match finish_return(regs, v) {
                    Ok(r) => r,
                    Err(v) => return v,
                },
            };
            // An error in the compiled code: unwind from where it left (its
            // frames have no handlers; a caller's may).
            v = if vm.error.is_some() {
                unwind_at(resume.pc, resume.regs)
            } else {
                dispatch_at(resume.pc, resume.acc, resume.regs)
            };
        }
    }
}

/// Finish frame `regs` returning `v` from compiled code, as `Return` does:
/// where the caller resumes, or `Err` with the result if the frame returns
/// to native code.
unsafe fn finish_return(regs: *mut Value, v: Value) -> Result<Resume, Value> {
    unsafe {
        let v = if (*regs.sub(FLAGS)).to_bits() & CONSTRUCT != 0 {
            construct_result(regs, v)
        } else {
            v
        };
        let ret: *const u8 = header(regs, RET_PC);
        if ret.is_null() {
            return Err(v);
        }
        Ok(Resume {
            regs: header(regs, CALLER),
            pc: ret,
            acc: v,
        })
    }
}

/// How [`enter_compiled`] ended, for the call handlers.
#[repr(C)]
struct Entered {
    kind: u64,
    regs: *mut Value,
    pc: *const u8,
    acc: Value,
}
const ENTER_INTERPRET: u64 = 0;
const ENTER_RETURNED: u64 = 1;
const ENTER_RESUME: u64 = 2;
const ENTER_ERROR: u64 = 3;

/// A call to a hot function (its call countdown ran out): run its compiled
/// code for the new frame `regs`. Returned: the result, for the caller
/// (`construct_result` applied). Exited: where the interpreter resumes.
/// Not compiled: the function won't be entered compiled again.
#[inline(never)]
fn enter_compiled(regs: *mut Value) -> Entered {
    unsafe {
        let vm = &mut *frame_vm(regs);
        let code = &*frame_code(regs);
        let done = |kind, acc| Entered {
            kind,
            regs,
            pc: std::ptr::null(),
            acc,
        };
        match crate::jit::vm_run(vm, code, regs, 0, Value::UNDEFINED) {
            VmRun::NotCompiled => {
                code.calls_left.set(i32::MAX);
                done(ENTER_INTERPRET, Value::UNDEFINED)
            }
            VmRun::Interpret => done(ENTER_INTERPRET, Value::UNDEFINED),
            VmRun::Returned(v) => {
                let v = if (*regs.sub(FLAGS)).to_bits() & CONSTRUCT != 0 {
                    construct_result(regs, v)
                } else {
                    v
                };
                done(ENTER_RETURNED, v)
            }
            VmRun::Exit(_) if vm.error.is_some() => done(ENTER_ERROR, Value::EMPTY),
            VmRun::Exit(r) => Entered {
                kind: ENTER_RESUME,
                regs: r.regs,
                pc: r.pc,
                acc: r.acc,
            },
        }
    }
}

/// Where the interpreter resumes after compiled code exits.
pub(crate) struct Resume {
    pub regs: *mut Value,
    pub pc: *const u8,
    pub acc: Value,
}

/// Write the header of a frame of `code` at `regs`, as `push_frame` does:
/// it returns to `ret` (null: to native code) in the frame at `caller`.
///
/// # Safety
/// `regs` must have room for the header below it, in `vm`'s stack.
pub(crate) unsafe fn write_frame_header(
    vm: &Vm,
    regs: *mut Value,
    code: *const Code,
    ret: *const u8,
    caller: *mut Value,
    flags: u64,
) {
    unsafe {
        *regs.sub(BUDGET) = Value::from_bits((*code).loop_budget.get() as u64);
        set_header(regs, CODE, code);
        set_header(regs, ICS, (*code).ics.as_ptr());
        set_header(regs, VM, vm as *const Vm);
        set_header(regs, RET_PC, ret);
        set_header(regs, CALLER, caller);
        *regs.sub(FLAGS) = Value::from_bits(flags);
    }
}

/// Native stack a run lets calls from compiled code use (each runs its
/// callee to completion on the native stack). Past it, the interpreter
/// makes them, in its own frames. Thread stacks are at least 2 MiB (Rust's
/// default for spawned threads).
pub(crate) const NATIVE_STACK: usize = 1 << 20;

/// Whether the native stack is past the run's limit (`Vm::native_limit`).
#[inline(always)]
pub(crate) fn native_stack_low(vm: &Vm) -> bool {
    let here = 0u8;
    (&raw const here as usize) < vm.native_limit
}

/// A call from compiled code (`jit`'s `call` stencil): `op` is `Call`,
/// `CallUndefined` or `Construct` with operands `f`, `w`, `argc` over the
/// compiled frame `regs`, whose registers it reads from memory. A bytecode
/// callee runs to completion in a new frame `frame` values past `regs`
/// (beyond every register the compiled code uses), through [`drive`], so
/// its loops can tier up too. Returns the result, or `EMPTY` on an error
/// (left in `vm.error`), or `EMPTY` with `vm.redo_call` set if the native
/// stack is low ([`native_stack_low`]): the compiled code then leaves, to
/// make the call in the interpreter.
///
/// # Safety
/// `regs` must be a live VM frame whose registers cover the operands.
pub(crate) unsafe fn call_from_jit(
    vm: &mut Vm,
    regs: *mut Value,
    op: Op,
    f: usize,
    w: usize,
    argc: usize,
    frame: usize,
) -> Value {
    unsafe {
        let mut fv = *regs.add(f);
        let (mut this, mut args, mut argc) = match op {
            Op::Call => (*regs.add(w), regs.add(w + 1), argc),
            _ => (object_value(vm.global), regs.add(w), argc),
        };
        let bytecode = fv.is_cell() && (*as_object(fv)).class == Class::Function;
        // `f.apply(that, list)`'s arguments (alive until the call returns).
        let spread: Vec<Value>;
        if !bytecode {
            // `f.call(that, …args)` with `f` a bytecode function: call `f`;
            // `f.apply(that, list)`: call `f` with `list`'s elements.
            if op == Op::Call
                && let Some(fo) = unwrap_apply(vm, fv, this)
            {
                let list = if argc < 2 { Value::UNDEFINED } else { *args.add(1) };
                spread = match crate::builtins::apply_list(list) {
                    Ok(s) => s,
                    Err(e) => return throw(vm, e),
                };
                fv = object_value(fo);
                this = if argc == 0 { Value::UNDEFINED } else { *args };
                args = spread.as_ptr().cast_mut();
                argc = spread.len();
            } else {
            match unwrap_call(vm, fv, this) {
                Some(fo) if op == Op::Call => {
                    fv = object_value(fo);
                    this = sloppy_this(vm, if argc == 0 { Value::UNDEFINED } else { *args });
                    args = args.add(1);
                    argc = argc.saturating_sub(1);
                }
                _ => {
                    // (Past every register the compiled code uses.)
                    let saved = vm.native_top;
                    vm.native_top = regs.add(frame - HEADER);
                    let r = if op == Op::Construct {
                        vm.construct_other(fv, std::slice::from_raw_parts(args, argc))
                    } else {
                        vm.call_other(fv, this, std::slice::from_raw_parts(args, argc))
                    };
                    vm.native_top = saved;
                    return r.unwrap_or_else(|e| throw(vm, e));
                }
            }
            }
        }
        let fo = as_object(fv);
        let callee = (*fo).code;
        // (A `Call` the JIT made of `f.call(that, …)` passes `that`.)
        let this = if op == Op::Construct {
            vm.construct_this(fo)
        } else {
            sloppy_this(vm, this)
        };
        let new = regs.add(frame);
        let n = (*callee).nregs as usize;
        if new.add(n) > vm.stack_end {
            return throw(vm, Error::range_error("Maximum call stack size exceeded"));
        }
        // Too deep in native calls: the compiled code leaves, before the
        // call (`EMPTY` without an error), and the interpreter makes it,
        // in its own frames.
        if native_stack_low(vm) {
            vm.redo_call = true;
            return Value::EMPTY;
        }
        let flags = if op == Op::Construct { CONSTRUCT } else { 0 };
        write_frame_header(vm, new, callee, std::ptr::null(), regs, flags);
        set_header(new, FUNC, fo);
        *new = this;
        let k = argc.min((*callee).nparams as usize);
        for i in 0..k {
            *new.add(1 + i) = *args.add(i);
        }
        for i in 1 + k..n {
            *new.add(i) = Value::UNDEFINED;
        }
        // For a `CreateArguments` (the frame has no call instruction).
        vm.pending_args = (args, argc);
        run_new_frame(vm, callee, new, flags)
    }
}

/// Run the frame `new` of `callee`, set up by the runtime (no return
/// address: it returns to here), to completion: in its compiled code if
/// it has some, else in the interpreter. The result, or `EMPTY` on an
/// error (left in `vm.error`).
///
/// # Safety
/// `new` must be a frame of `callee` with its header and registers set.
unsafe fn run_new_frame(vm: &mut Vm, callee: *const Code, new: *mut Value, flags: u64) -> Value {
    unsafe {
        let mut start = Resume {
            regs: new,
            pc: (*callee).bytecode.as_ptr(),
            acc: Value::UNDEFINED,
        };
        // A hot callee runs compiled; an exit resumes in the interpreter.
        // So does one compiled before (for direct calls, say: a function
        // without loops has no countdown, and a site that stopped calling
        // it directly waits for it to compile again).
        let left = (*callee).calls_left.get() - 1;
        (*callee).calls_left.set(left);
        if left < 0 || (*callee).jit.borrow().is_some() {
            match crate::jit::vm_run(vm, &*callee, new, 0, Value::UNDEFINED) {
                VmRun::NotCompiled => (*callee).calls_left.set(i32::MAX),
                VmRun::Interpret => {}
                VmRun::Returned(v) => {
                    return if flags == CONSTRUCT {
                        construct_result(new, v)
                    } else {
                        v
                    };
                }
                VmRun::Exit(_) if vm.error.is_some() => {
                    return Value::EMPTY;
                }
                VmRun::Exit(r) => start = r,
            }
        }
        drive(vm, start.pc, start.acc, start.regs)
    }
}

/// `op_unwind` from `pc` of frame `regs`, called from outside a handler.
unsafe fn unwind_at(pc: *const u8, regs: *mut Value) -> Value {
    let tbl: &'static Table = std::hint::black_box(&TABLE);
    let ti32: &'static Table = std::hint::black_box(&TABLE_I32);
    op_unwind(pc, Value::EMPTY, regs, NumberTag::new(), tbl, ti32)
}

/// Run the interpreter from `pc` with `acc` over the VM frame `regs`, until
/// the outermost frame returns, an error, or a tier-up request (`EMPTY`).
unsafe fn dispatch_at(pc: *const u8, acc: Value, regs: *mut Value) -> Value {
    let tbl: &'static Table = std::hint::black_box(&TABLE);
    let ti32: &'static Table = std::hint::black_box(&TABLE_I32);
    let tag = NumberTag::new();
    unsafe { (tbl.0[*pc as usize])(pc, acc, regs, tag, tbl, ti32) }
}
