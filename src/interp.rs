//! The tail-calling dispatcher.
//!
//! Interpreter state lives entirely in arguments, so with `preserve_none` it
//! lives entirely in registers:
//!
//! | arg    | meaning                                 |
//! |--------|-----------------------------------------|
//! | `pc`   | pointer to the current instruction      |
//! | `acc`  | the accumulator                         |
//! | `regs` | pointer to the register file            |
//! | `vm`   | VM context (heap, constants, …) — placeholder for now |
//! | `tbl`  | the handler table (kept in a register so dispatch needs no `lea`) |
//!
//! Argument order matters. `preserve_none` passes args 1–4 in r12–r15 and
//! arg 5 in rdi. LLVM's x86 backend only lets the address of an indirect tail
//! call's memory operand use registers in `GR64_TC` (caller-saved under the C
//! ABI), which excludes r12–r15. With `tbl` in r15 every handler paid an
//! extra `mov` to copy it out. In rdi, `jmp [rdi + rcx*8]` works directly.
//!
//! Dispatch to the next instruction is: load opcode byte, advance `pc`,
//! indirect jump through `tbl`.

use crate::bytecode::Op;

pub type Handler =
    extern "rust-preserve-none" fn(*const u8, i64, *mut i64, *mut Vm, &'static Table) -> i64;

/// Per-VM state reachable from every handler. Empty until there is a heap.
pub struct Vm {}

#[repr(transparent)]
pub struct Table([Handler; 256]);

/// Tail-call the handler of the instruction at `$pc + $len`.
///
/// The empty `asm!` emits no code. It does two things for the register
/// allocator:
/// - It is a scheduling barrier: not `pure`, so it can't be reordered. Without
///   it the scheduler hoists the next-opcode load above the operand reads.
/// - It launders `next` through a register, so LLVM can't fold `pc + off` into
///   the opcode load's addressing mode (`movzx ecx, [r12 + rcx]`).
///
/// Either one keeps the old and new `pc` live together, and LLVM then emits
/// `lea rax,[r12+N] … mov r12,rax` where one `add r12,N` would do.
/// `nomem` keeps it from acting as a compiler memory fence.
macro_rules! dispatch {
    ($pc:expr, $len:expr, $acc:expr, $regs:expr, $vm:expr, $tbl:expr) => {{
        let mut next: *const u8 = $pc.add($len);
        core::arch::asm!("/* {0} */", inout(reg) next, options(nomem, nostack, preserves_flags));
        let op = *next;
        become ($tbl.0[op as usize])(next, $acc, $regs, $vm, $tbl)
    }};
}

/// Read an unaligned little-endian `i32` operand at `pc + off`.
#[inline(always)]
unsafe fn imm32(pc: *const u8, off: usize) -> i32 {
    unsafe { i32::from_le(pc.add(off).cast::<i32>().read_unaligned()) }
}

/// Read a register operand at `pc + off` and return a pointer to that register.
#[inline(always)]
unsafe fn reg(pc: *const u8, off: usize, regs: *mut i64) -> *mut i64 {
    unsafe { regs.add(*pc.add(off) as usize) }
}

macro_rules! handlers {
    ($( $(#[$m:meta])* fn $name:ident($pc:ident, $acc:ident, $regs:ident, $vm:ident, $tbl:ident) $body:block )*) => {$(
        $(#[$m])*
        #[unsafe(no_mangle)]
        #[allow(unused_assignments)]
        // `dispatch!` hands `next` to a `nomem` asm block, which never dereferences it.
        #[allow(clippy::pointers_in_nomem_asm_block)]
        extern "rust-preserve-none" fn $name(
            $pc: *const u8,
            #[allow(unused_mut)] mut $acc: i64,
            $regs: *mut i64,
            $vm: *mut Vm,
            $tbl: &'static Table,
        ) -> i64 {
            #[allow(unused_unsafe)]
            unsafe { $body }
        }
    )*};
}

handlers! {
    fn toi_op_nop(pc, acc, regs, vm, tbl) {
        dispatch!(pc, Op::Nop.size(), acc, regs, vm, tbl)
    }

    fn toi_op_lda_smi(pc, acc, regs, vm, tbl) {
        acc = imm32(pc, 1) as i64;
        dispatch!(pc, Op::LdaSmi.size(), acc, regs, vm, tbl)
    }

    fn toi_op_ldar(pc, acc, regs, vm, tbl) {
        acc = *reg(pc, 1, regs);
        dispatch!(pc, Op::Ldar.size(), acc, regs, vm, tbl)
    }

    fn toi_op_star(pc, acc, regs, vm, tbl) {
        *reg(pc, 1, regs) = acc;
        dispatch!(pc, Op::Star.size(), acc, regs, vm, tbl)
    }

    fn toi_op_add(pc, acc, regs, vm, tbl) {
        acc = acc.wrapping_add(*reg(pc, 1, regs));
        dispatch!(pc, Op::Add.size(), acc, regs, vm, tbl)
    }

    fn toi_op_inc(pc, acc, regs, vm, tbl) {
        acc = acc.wrapping_add(1);
        dispatch!(pc, Op::Inc.size(), acc, regs, vm, tbl)
    }

    fn toi_op_jump_if_less(pc, acc, regs, vm, tbl) {
        let next = if acc < *reg(pc, 1, regs) {
            pc.offset(imm32(pc, 2) as isize)
        } else {
            pc.add(Op::JumpIfLess.size())
        };
        dispatch!(next, 0, acc, regs, vm, tbl)
    }

    fn toi_op_jump(pc, acc, regs, vm, tbl) {
        let next = pc.offset(imm32(pc, 1) as isize);
        dispatch!(next, 0, acc, regs, vm, tbl)
    }

    fn toi_op_return(pc, acc, regs, vm, tbl) {
        let _ = (pc, regs, vm, tbl);
        acc
    }

    #[cold]
    fn toi_op_invalid(pc, acc, regs, vm, tbl) {
        let _ = (pc, acc, regs, vm, tbl);
        std::process::abort()
    }
}

static TABLE: Table = {
    let mut t: [Handler; 256] = [toi_op_invalid; 256];
    t[Op::Nop as usize] = toi_op_nop;
    t[Op::LdaSmi as usize] = toi_op_lda_smi;
    t[Op::Ldar as usize] = toi_op_ldar;
    t[Op::Star as usize] = toi_op_star;
    t[Op::Add as usize] = toi_op_add;
    t[Op::Inc as usize] = toi_op_inc;
    t[Op::JumpIfLess as usize] = toi_op_jump_if_less;
    t[Op::Jump as usize] = toi_op_jump;
    t[Op::Return as usize] = toi_op_return;
    Table(t)
};

/// Run `code` with a register file of `nregs` zeroed registers and return the
/// accumulator at `Return`.
///
/// # Safety
/// `code` must be well formed: every instruction complete, every register
/// operand `< nregs`, every jump landing on an instruction boundary, and every
/// path ending in `Return`. There is no verifier yet.
pub unsafe fn run(code: &[u8], nregs: usize) -> i64 {
    let mut regs = vec![0i64; nregs];
    let mut vm = Vm {};
    let pc = code.as_ptr();
    // Launder the table address so LLVM can't fold it back into a
    // RIP-relative `lea` in every handler; it must stay a register argument.
    let tbl: &'static Table = std::hint::black_box(&TABLE);
    unsafe { (tbl.0[*pc as usize])(pc, 0, regs.as_mut_ptr(), &mut vm, tbl) }
}
