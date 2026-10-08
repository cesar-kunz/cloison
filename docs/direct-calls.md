# Direct calls: what they cost, and a cheaper convention

Status: design, 2026-10-08. Nothing here is implemented yet.

## Why

Since compiled code calls compiled code directly, polymorphic sites
included, the runtime no longer shows up in long DeltaBlue runs. The
protocol of the direct call itself does: in 1000 calls of `deltaBlue()`
(333 ms), the `toi_rt_enter` wrapper is about 13% of the cycles and
`call_direct` about 4%. fib (90) is nothing but calls: 32M instructions for
fib(27), at 4.6 per cycle.

## A direct call today (x86-64, fib)

Caller, `call_direct` (`stencils/stencils.rs`), with the arguments already
stored into the callee's frame (two `mov [r13+…]`, by `direct_setup`):

```text
push rax                         ; LLVM keeps a value across the call
movabs r11, site
cmp [r11], 0 ; je runtime        ; entry bound?
mov rax, [r11+0x10] ; mov rax, [r13+rax] ; cmp rax, [r11+8] ; jne runtime
                                 ; the callee register holds the closure?
mov rax, [r11+0x18] ; add rax, r13 ; cmp rax, [r11+0x20] ; ja runtime
                                 ; the VM stack has room?
mov rax, r13 ; lea rsp, [rsp-0x80] ; call wrapper   ; past the red zone
lea rsp, [rsp+0x80]
test rax, rax ; je error
movabs r10, &code_epoch ; mov r11d, epoch ; cmp [r10], r11d ; jne exit
mov r12, rax ; pop rax
```

The region's wrapper for `toi_rt_enter` (`arch::enter_wrapper`):

```text
push rcx ; push rdx ; push rsi ; push rdi ; push r8   ; the live slots
mov r10, [r11+0x28] ; cmp rsp, [r10] ; jb slow       ; native stack
push rbx ; push rbp ; push r11 ; push r13
mov r10, [r11+0x68] ; lea r13, [rax+r10*8]           ; callee frame
mov qword [r13-0x48], 0                              ; flags
mov r12d, undefined
call [r11]                                           ; the entry
pop r13 ; pop r11 ; pop rbp ; pop rbx
cmp dword [r15], NO_EXIT ; jne exit
pop r8 ; pop rdi ; pop rsi ; pop rdx ; pop rcx ; ret
```

The callee loads its parameters from its frame at entry, returns its
result in `rax` (`mov rax, r12; ret`).

That's about 60 instructions per call besides the body: 11 loads from the
site, 5 + 4 pushes and pops, two calls through a register or memory, and
four checks (bound, closure, VM stack, native stack) plus two after it
(exit, code epoch). Arguments go through memory both ways.

## Options, cheapest first

Each is a separate step, measured with `perf.sh --ab` (fib, mutual
recursion, 18, 87) and DeltaBlue ×1000.

1. **No closure check where the callee is proven.** Behind a callee guard
   (polymorphic dispatch, task 16) or a `proven_callee`, the check repeats
   the guard. A `call_direct_nc` variant: −3 instructions, −2 loads.
2. **No `push r11` / `push r13` in the wrapper.** r13 is the caller's frame,
   `lea r13, [r13 - frame*8]` after the call recovers it; r11 (the site)
   is needed only on the exit path, which can reload it with a `movabs`.
   −4.
3. **The exit in the result.** The callee returns `EMPTY` (0) when it
   exited (the cell still records how), so the wrapper's `cmp [r15]` goes,
   and the caller's `test rax, rax` covers errors and exits alike. −2.
4. **Bound sites as immediates.** `entry`, `end_off` and the closure are
   read from the site because a site is bound after the caller is compiled
   (mutual recursion) and unbound after callee exits. When the site is
   bound at compile time, and stays bound for the code's life (an unbind
   makes the caller exit and recompile instead), they become immediates:
   `call rel32` to the entry (JIT code is carved from one 1 GiB
   reservation, `EXEC_RANGE`, so `rel32` reaches), no `entry` test.
   −6 loads, an indirect call becomes a direct one.
5. **Callee-saved slots.** Today the callee may use every register, so the
   caller saves every live slot (5 in fib). If the callee saved, in its
   prologue, the slots it uses, a leaf callee using two slots would save
   two. This changes the stencil ABI (`rbx`, used by 79 stencils, and the
   slot registers), and the entry and every exit path of every compiled
   function. Largest saving on calls between small functions, largest
   change.
6. **Arguments in registers.** The callee loads its parameters from its
   frame at entry; the caller just stored them there. Passing up to N of
   them in the slot registers the callee assigns them to (known when
   binding: the callee's version is compiled first) saves a store and a load
   each. Exits must spill them to the frame first, as for any slot.

## Recommendation

Steps 1–3 are local to `call_direct`, the wrapper and the callee's return,
on both architectures, about 30 lines each: do them first, measure each.
Step 4 changes when sites bind (an unbind must invalidate callers), a
medium change. Steps 5 and 6 change the calling convention of all
compiled code; take them on only if 1–4 leave the call protocol a large
share of DeltaBlue's time.
