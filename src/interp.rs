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
//! | `vm`   | r15    | x23   | VM context (heap, constants, …) — placeholder for now |
//! | `tbl`  | rdi    | x24   | the handler table                        |
//! | `tag`  | rsi    | x25   | [`NumberTag`], pinned like JSC's `numberTag` register |
//!
//! Argument order matters on x86-64. LLVM's x86 backend only lets the address
//! of an indirect tail call's memory operand use registers in `GR64_TC`
//! (caller-saved under the C ABI), which excludes r12–r15. With `tbl` in r15
//! every handler paid an extra `mov` to copy it out. In rdi,
//! `jmp [rdi + rcx*8]` works directly. On arm64 the order doesn't matter;
//! `tbl` only feeds an `ldr`.
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

use crate::bytecode::Op;
use crate::value::Value;

pub type Handler = extern "rust-preserve-none" fn(
    *const u8,
    Value,
    *mut Value,
    *mut Vm,
    &'static Table,
    NumberTag,
) -> Value;

/// Per-VM state reachable from every handler. Empty until there is a heap.
pub struct Vm {}

#[repr(transparent)]
pub struct Table([Handler; 256]);

/// [`Value::NUMBER_TAG`], kept in a register for the whole run.
///
/// x86 instructions can't take a 64-bit immediate. With the tag as a
/// constant, every int32 test becomes `shr` + `cmp`, and every int32 box a
/// `movabs`. JSC pins it in a register for the same reason. It's passed as a
/// handler argument, laundered once in [`run`], so LLVM can't
/// constant-fold it back in.
#[repr(transparent)]
#[derive(Clone, Copy)]
pub struct NumberTag(u64);

impl NumberTag {
    #[inline(always)]
    fn is_int32(self, v: Value) -> bool {
        v.to_bits() >= self.0
    }

    /// See [`Value::both_int32`].
    #[inline(always)]
    fn both_int32(self, a: Value, b: Value) -> bool {
        a.to_bits() & b.to_bits() >= self.0
    }

    #[inline(always)]
    fn box_i32(self, i: i32) -> Value {
        Value::from_bits(self.0 | i as u32 as u64)
    }
}

/// Tail-call the handler of the instruction at `$pc + $off` (bytes, signed).
macro_rules! dispatch {
    ($pc:expr, $off:expr, $acc:expr, $regs:expr, $vm:expr, $tbl:expr, $tag:expr) => {{
        let next: *const u8 = $pc.offset($off as isize);
        pc_barrier!(next);
        let op = *next;
        become ($tbl.0[op as usize])(next, $acc, $regs, $vm, $tbl, $tag)
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

macro_rules! handlers {
    ($( $(#[$m:meta])* fn $name:ident($pc:ident, $acc:ident, $regs:ident, $vm:ident, $tbl:ident, $tag:ident) $body:block )*) => {$(
        $(#[$m])*
        #[allow(unused_assignments)]
        // `dispatch!` hands `next` to a `nomem` asm block, which never dereferences it.
        #[allow(clippy::pointers_in_nomem_asm_block)]
        extern "rust-preserve-none" fn $name(
            $pc: *const u8,
            #[allow(unused_mut)] mut $acc: Value,
            $regs: *mut Value,
            $vm: *mut Vm,
            $tbl: &'static Table,
            $tag: NumberTag,
        ) -> Value {
            #[allow(unused_unsafe)]
            unsafe { $body }
        }
    )*};
}

handlers! {
    fn op_nop(pc, acc, regs, vm, tbl, tag) {
        dispatch!(pc, Op::Nop.size(), acc, regs, vm, tbl, tag)
    }

    fn op_lda_smi(pc, acc, regs, vm, tbl, tag) {
        acc = tag.box_i32(imm32(pc, 1));
        dispatch!(pc, Op::LdaSmi.size(), acc, regs, vm, tbl, tag)
    }

    fn op_ldar(pc, acc, regs, vm, tbl, tag) {
        acc = *reg(pc, 1, regs);
        dispatch!(pc, Op::Ldar.size(), acc, regs, vm, tbl, tag)
    }

    fn op_star(pc, acc, regs, vm, tbl, tag) {
        *reg(pc, 1, regs) = acc;
        dispatch!(pc, Op::Star.size(), acc, regs, vm, tbl, tag)
    }

    fn op_add(pc, acc, regs, vm, tbl, tag) {
        let rhs = *reg(pc, 1, regs);
        if tag.both_int32(acc, rhs)
            && let Some(sum) = acc.as_i32_unchecked().checked_add(rhs.as_i32_unchecked())
        {
            dispatch!(pc, Op::Add.size(), tag.box_i32(sum), regs, vm, tbl, tag)
        }
        become slow_add(pc, acc, regs, vm, tbl, tag)
    }

    fn op_inc(pc, acc, regs, vm, tbl, tag) {
        if tag.is_int32(acc)
            && let Some(n) = acc.as_i32_unchecked().checked_add(1)
        {
            dispatch!(pc, Op::Inc.size(), tag.box_i32(n), regs, vm, tbl, tag)
        }
        become slow_inc(pc, acc, regs, vm, tbl, tag)
    }

    fn op_jump_if_less(pc, acc, regs, vm, tbl, tag) {
        let rhs = *reg(pc, 1, regs);
        if !tag.both_int32(acc, rhs) {
            become slow_jump_if_less(pc, acc, regs, vm, tbl, tag)
        }
        // Two separate dispatches, not one on a selected pc. A select
        // materializes the fall-through length (`mov eax, 6`) and blocks the
        // pre-indexed load on arm64.
        if acc.as_i32_unchecked() < rhs.as_i32_unchecked() {
            dispatch!(pc, imm32(pc, 2), acc, regs, vm, tbl, tag)
        } else {
            dispatch!(pc, Op::JumpIfLess.size(), acc, regs, vm, tbl, tag)
        }
    }

    fn op_jump(pc, acc, regs, vm, tbl, tag) {
        dispatch!(pc, imm32(pc, 1), acc, regs, vm, tbl, tag)
    }

    fn op_return(pc, acc, regs, vm, tbl, tag) {
        let _ = (pc, regs, vm, tbl, tag);
        acc
    }

    #[cold]
    fn op_invalid(pc, acc, regs, vm, tbl, tag) {
        let _ = (pc, acc, regs, vm, tbl, tag);
        std::process::abort()
    }

    // Slow paths (tier 2). Same signature as the handlers so they can be
    // `become`d. `inline(never)` keeps LLVM from merging them back in.

    #[cold]
    #[inline(never)]
    fn slow_add(pc, acc, regs, vm, tbl, tag) {
        let rhs = *reg(pc, 1, regs);
        acc = match both_numbers(acc, rhs) {
            Some((a, b)) => Value::from_f64_arith(a + b),
            None => generic_add(acc, rhs),
        };
        dispatch!(pc, Op::Add.size(), acc, regs, vm, tbl, tag)
    }

    #[cold]
    #[inline(never)]
    fn slow_inc(pc, acc, regs, vm, tbl, tag) {
        acc = match acc.as_number() {
            Some(a) => Value::from_f64_arith(a + 1.0),
            None => generic_inc(acc),
        };
        dispatch!(pc, Op::Inc.size(), acc, regs, vm, tbl, tag)
    }

    #[cold]
    #[inline(never)]
    fn slow_jump_if_less(pc, acc, regs, vm, tbl, tag) {
        let rhs = *reg(pc, 1, regs);
        let less = match both_numbers(acc, rhs) {
            // False if either is NaN.
            Some((a, b)) => a < b,
            None => generic_less_than(acc, rhs),
        };
        if less {
            dispatch!(pc, imm32(pc, 2), acc, regs, vm, tbl, tag)
        } else {
            dispatch!(pc, Op::JumpIfLess.size(), acc, regs, vm, tbl, tag)
        }
    }
}

// Generic runtime (tier 3): ordinary Rust functions, called (not tail-called)
// from the slow paths. Here go ToPrimitive, strings, BigInt and exceptions
// once they exist; they'll take the `Vm`. A call from a `preserve_none`
// handler is cheap: the callee preserves r12–r15 / x19–x28, where most of
// the interpreter state already lives.

/// `a + b` where at least one side is not a number.
#[cold]
#[inline(never)]
fn generic_add(a: Value, b: Value) -> Value {
    // ToPrimitive is the identity on everything representable so far, and
    // there are no strings yet, so `+` is numeric addition.
    Value::number(a.to_number() + b.to_number())
}

/// `a + 1` where `a` is not a number.
#[cold]
#[inline(never)]
fn generic_inc(a: Value) -> Value {
    Value::number(a.to_number() + 1.0)
}

/// `a < b` (IsLessThan) where at least one side is not a number.
#[cold]
#[inline(never)]
fn generic_less_than(a: Value, b: Value) -> bool {
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
    t[Op::Return as usize] = op_return;
    Table(t)
};

/// Run `code` over the register file `regs` and return the accumulator at
/// `Return`. The accumulator starts as `undefined`.
///
/// # Safety
/// `code` must be well formed: every instruction complete, every register
/// operand `< regs.len()`, every jump landing on an instruction boundary, and
/// every path ending in `Return`. There is no verifier yet.
pub unsafe fn run(code: &[u8], regs: &mut [Value]) -> Value {
    let mut vm = Vm {};
    let pc = code.as_ptr();
    // Launder the table address and the number tag so LLVM can't fold them
    // back into constants (`lea`/`movabs`) in every handler. They must stay
    // register arguments.
    let tbl: &'static Table = std::hint::black_box(&TABLE);
    let tag = NumberTag(std::hint::black_box(Value::NUMBER_TAG));
    let acc = Value::UNDEFINED;
    unsafe { (tbl.0[*pc as usize])(pc, acc, regs.as_mut_ptr(), &mut vm, tbl, tag) }
}
