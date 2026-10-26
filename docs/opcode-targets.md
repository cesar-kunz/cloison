# Opcode targets: ideal handwritten code vs. what the JIT emits

For every opcode added or reworked, the ideal sequence a person would write
for its hot cases, on x86-64 and arm64, then what the JIT actually emits,
so the gap stays visible. Each section is dated and describes the code as
it was then; `docs/arm64-targets.md` continues it on arm64. The targets set
here are in the code as `// target <arch> <key>: <n>` comments next to the
stencils (`stencils/stencils.rs`), which `scripts/stencil-targets.py`
checks.

Conventions (see `stencils/stencils.rs`):

| | x86-64 | arm64 |
|---|---|---|
| `acc` (int32: low 32 bits, high bits 0 or tag) | r12 | x20 |
| `regs` / `tag` / `ctx` | r13 / r14 / r15 | x21 / x22 / x23 |
| slots `u<k>`/`g<k>` (raw int32 zero-extended / boxed) | rdi rsi rdx rcx r8 r9 | x24–x28, x0–x6 |
| `facc`, `f<k>` (doubles) | xmm0, xmm1–7 | d0, d1–7 |
| scratch | rax r10 r11 rbx, xmm8–15 | x8–x17, d16–d31 |

Stencils are plain Rust by default; inline asm where LLVM can't reach the
target and that costs speed (speed comes first; noted per opcode).

Toolchain regressions: `scripts/stencil-snapshot.sh [aarch64]` records every
stencil's code under `notes/stencils/llvm-<version>-nightly-<date>/`, and
`--compare` / `--diff NAME` show what a new LLVM changed. Take a snapshot
after each `rustup update` (rust-toolchain.toml follows nightly) and check
the stencils this document targets.

Below, `a` is a slot holding the register operand (esi / w25), `L` a jump
target, `exit` a side exit (whose stub may undo an in-place result).
Values: null = 0x2, undefined = 0xA, false = 0x6, true = 0x7.

---

## Group 1 (2026-10-07): arithmetic, comparisons, strict equality

### SubSmi k — `acc - k`

Ideal (int32 acc), in place, the exit stub undoes it (`add k`):

    x86-64:  sub r12d, k ; jo exit
    arm64:   subs w20, w20, #k ; b.vs exit        (k < 4096; else movz w16,#k first)

`x -= k` on an int32 slot (`ldar x; SubSmi k; star x`): `sub esi, k; jo exit`
/ `subs w25, w25, #k; b.vs exit`.

Plan: the JIT treats `SubSmi k` as `AddSmi -k` for k ≠ INT32_MIN (same
instruction count: `add r12d, -k`), so every `AddSmi` fusion applies
(`addi_u`, the loop tails). Interpreter: its own handlers.

### Sub r — `r - acc`

Ideal (both int32):

    x86-64:  mov eax, esi ; sub eax, r12d ; jo exit ; mov r12d, eax
    arm64:   subs w20, w25, w20 ; b.vs exit       (in place; the stub undoes: acc = a - acc)

`x = x - y` with both in int32 slots (`ldar y; Sub x; star x`), in place:

    x86-64:  sub esi, edi ; jo exit
    arm64:   subs w25, w25, w24 ; b.vs exit

Doubles: `subsd`. Plan: `sub_{ii,iif,if,fi,ff}_<loc>`, generic `sub_<loc>`
(runtime `cloison_rt_sub`), fused `subr_ii_u<a>_u<b>` with undo.

### MulSmi k — `acc * k`

Ideal (int32 acc), k > 0 (no -0 possible):

    x86-64:  imul eax, r12d, k ; jo exit ; mov r12d, eax
    arm64:   movz w16, #k ; smull x17, w20, w16 ; cmp x17, w17, sxtw ; b.ne exit ; mov w20, w17

k < 0 adds `test eax, eax; jz exit` (0 · k is -0); k = 0 is `test r12d,
r12d; js exit; xor r12d, r12d`. Doubles: `mulsd` with k converted.

`x *= k` on a slot: `imul eax, esi, k; jo exit; mov esi, eax`.

### Mul r — `r * acc`

Ideal (both int32): the product, and on a zero product a sign check
(`0 · -n` is -0), out of line:

    x86-64:  mov eax, r12d ; imul eax, esi ; jo exit ; test eax, eax ; jz zero ; mov r12d, eax
             zero: mov r10d, r12d ; or r10d, esi ; js exit ; xor r12d, r12d
    arm64:   smull x16, w20, w25 ; cmp x16, w16, sxtw ; b.ne exit ; cbz w16, zero ; mov w20, w16

### Div r — `r / acc`, DivSmi k — `acc / k`

Always a double (exact int quotients included):

    x86-64 (ii):  cvtsi2sd xmm0, esi ; cvtsi2sd xmm8, r12d ; divsd xmm0, xmm8
    arm64  (ii):  scvtf d0, w25 ; scvtf d16, w20 ; fdiv d0, d0, d16
    DivSmi k:     cvtsi2sd xmm0, r12d ; mov eax, k ; cvtsi2sd xmm8, eax ; divsd xmm0, xmm8
    (ff)          divsd xmm0, xmm1 ...

(`cvtsi2sd` has a false dependency on its destination; a `xorps` first
breaks it when the destination was just written by a long op.)

### ShiftLeftSmi / ShiftRightSmi / ShiftRightLogicalSmi k

int32 acc, in place, k taken mod 32:

    x86-64:  shl r12d, k   |  sar r12d, k   |  shr r12d, k
    arm64:   lsl w20, w20, #k | asr w20, w20, #k | lsr w20, w20, #k

`>>> 0` of a negative int32 is above 2^31: `test r12d, r12d; js exit`
(k = 0 only). Doubles: ToInt32 first (as `bit_and_smi_f`). On a slot
(`ldar a; ShiftRightSmi k; star b`): `mov`, shift in place on `b`.

### Comparisons: TestGreaterThan r (`r > acc`), TestLessThanOrEqual r (`r <= acc`), TestGreaterThanOrEqual r (`r >= acc`)

Fused with the conditional jump after them (the usual case), int32:

    x86-64:  cmp esi, r12d ; jg L        (jle for the false branch, …)
    arm64:   cmp w25, w20 ; b.gt L

Both operands in slots (`ldar b; Test… a; JumpIf…`): `cmp esi, edi; jcc L`.
Against a literal (`lda_smi k; Test… a; JumpIf…`, the codegen form of
`if (x > 3)`): `cmp esi, k; jcc L`.

As a value (boolean in acc):

    x86-64:  xor eax, eax ; cmp esi, r12d ; setg al ; lea r12, [rax + 6]
    arm64:   cmp w25, w20 ; cset w16, gt ; orr x20, x16, #6

Doubles: `ucomisd` + the unordered-safe condition (`a > b` as `ucomisd a,
b; ja`, `a < b` as `ucomisd b, a; ja`, never `jb`, which is true on NaN).

For int32 operands every relation reduces to `<` or its negation, with
the operands in either order, so the int32 compare-and-branch stencils
needed are `acc < r` (`jlt`), `r < acc` (`tlt`), and their negations.

### JumpIfLessEqualSmi / JumpIfGreaterSmi / JumpIfGreaterEqualSmi k, JumpIfLessEqual / JumpIfGreater / JumpIfGreaterEqual r (with loop forms)

`acc OP k` / `acc OP r` and jump: the loop conditions `i <= n`, `i >= 0`,
`todo.size() > 0`. int32:

    x86-64:  cmp r12d, k ; jle L         (jg, jge)
    arm64:   cmp w20, #k ; b.le L

For int32, `acc <= k` is `acc < k + 1` and `acc > k` is `!(acc < k + 1)`
(k < INT32_MAX), so with `jlt_smi_i` the new stencil is `jge_smi_i`.

### TestStrictEqual r — `r === acc`

int32 both: `cmp esi, r12d; je L`. Non-doubles: one 64-bit compare of the
boxed bits, `cmp rsi, r12; je L`. Doubles: `ucomisd` (NaN ≠ NaN, +0 = -0).

### TestUndefined / TestNull — `acc === undefined` / `acc === null`

    x86-64:  cmp r12, 10 ; je L            (cmp r12, 2)
    arm64:   cmp x20, #10 ; b.eq L

As a value: `xor eax, eax; cmp r12, 10; sete al; lea r12, [rax + 6]`. On a
slot, compare the slot (`ldar a` skipped). Nothing when the type says acc
is an object or a number (constant false).

### TestNullish — `acc == null` (null or undefined)

    x86-64:  lea rax, [r12 - 2] ; test rax, -9 ; jz L     ((x - 2) & ~8 == 0 ⇔ x ∈ {2, 10})
    arm64:   sub x16, x20, #2 ; tst x16, #0xfffffffffffffff7 ; b.eq L

Replaces `LdaNull; TestEqual r`, which went through the runtime's loose
equality for every object (66, 84: `prev != null`, `next == null`).

---

## Group 1 as emitted (2026-10-07, LLVM 23.1.3, nightly 2026-10-05)

Measured from `scripts/stencil-snapshot.sh` (x86-64 and aarch64). ✓ = the
target above.

| stencil | x86-64 | arm64 | gap |
|---|---|---|---|
| `SubSmi k` (as `add_smi_i -k`, `addi_u`) | `add r12d, -k; jo` ✓ | `adds`/`subs; b.vs` ✓ | — |
| `subr_ii_u<x>_u<y>` (`x = x - y`) | `sub esi, edi; jo` ✓ | `subs w25, w25, w24; b.vc` ✓ | — |
| `sub_ii_<loc>` (`r - acc`) | `mov eax, esi; sub eax, r12d; jo; mov r12d, eax` ✓ | `subs w8, w25, w20; b.vs; mov x20, x8` | arm64: +1 (`mov`); in place needs an undo stencil on a non-fused piece |
| `sub_iif` | `cvtsi2sd ×2; subsd` | `scvtf ×2; fsub` | — |
| `mul_smi_pos_i` | `imul eax, r12d, k; jo; mov r12d, eax` ✓ (asm) | `movz; smull; cmp sxtw; b.ne; mov` ✓ | — |
| `mul_smi_neg_i` | `imul; jo; test eax, eax; je exit; mov` ✓ (asm) | as pos + `cbz` | — |
| `mul_smi_zero_i` | `test r12d, r12d; js exit; xor r12d, r12d` ✓ | `tbnz; mov w20, wzr` | — |
| `mul_ii_<loc>` | `mov eax, r12d; imul eax, esi; jo; test eax, eax; je slow; mov r12d, eax` ✓ (sign check in `__slow`) | `smull; cmp sxtw; b.ne; cbz; mov` ✓ | — |
| `div_ii`, `div_smi_i` | `cvtsi2sd ×2; divsd` (+`mov eax, k`) ✓ | `scvtf ×2; fdiv` (+`movz/movk`) ✓ | no `xorps` against `cvtsi2sd`'s false dependency |
| `shl/sar/shr_smi_i_<k>` | `sar r12d, k` ✓ (count in the stencil name) | `asr w20, w20, #k` ✓ | — |
| `shr_smi_i_0` | `test r12d, r12d; js exit` ✓ | `tbnz w20, #31, exit` ✓ | — |
| `jge_smi_i` (`>=`; `<=`/`>` via k+1 on `jlt_smi_i`/`jge_smi_i`) | `cmp r12d, k; jge` ✓ (asm, as `jlt_smi_i`) | `movz w8, #k; cmp w20, w8; b.ge` | arm64: +1, as `jlt_smi_i` (a `cmp #imm12` needs a relocation for the field) |
| `jge_ii_<loc>`, `tlt_j*_ii`, `jlt_ii` (all int32 relations) | `cmp r12d, esi; jge` ✓ | `cmp w20, w25; b.ge` ✓ | — |
| `test_gt/le/ge_ii` (boolean) | `xor; cmp; setcc; or 6; mov` | `cmp; mov #6; cinc` ✓ | x86-64: +1 vs `lea r12, [rax+6]` (same as the existing `test_lt`) |
| slot pairs `ldar a; Test… b; JumpIf…` | `cmp esi, edi; jcc` ✓ (existing `tlt_*_u_u`, operands swapped/negated) | ✓ | — |
| `tund/tnull_j*`, `_g<k>` | `cmp r12, 10; jne` ✓ | `cmp x20, #10; b.ne` ✓ | — |
| `tnsh_j*` (`== null`) | `lea rax, [r12-2]; test rax, -9; je` ✓ | `sub; tst; b.eq` ✓ | — |
| `test_seq`/`tseq_*` (`===`, generic) | 3 tests + `cmp` + `jcc` | similar | generic only; int32 operands use `teq_*_ii` |
| doubles with `>`, `<=`, `>=` | through `jrel`/`test_rel` (runtime for doubles) | same | double compares for the new relations aren't specialized |

Not fused yet (each a few instructions over the ideal):
`lda_smi k; Test… a; JumpIf…` (`if (x > 3)`; ideal `cmp esi, 3; jle`),
`ldar a; JumpIf…Smi k` with `a` in a slot (an extra `mov`), `ldar a; SubSmi k;
star a; ldar a; JumpIfGreaterEqualSmi k` loop tails (the `incjlt` analogue),
`ldar a; MulSmi k; star b` (`imul ebx?, esi, k`), shifts from slot to slot.

### Size (x86-64, vs bd102e0)

| | before | after | |
|---|---:|---:|---:|
| stencils.text | 41 261 | 49 686 | +20% |
| runtime .text | 595 152 | 614 720 | +3% |
| runtime .rodata | 117 711 | 143 676 | +22% |
| runtime .data.rel.ro | 137 952 | 172 296 | +25% |
| JIT code, all listings | 14 518 | 14 518 | 0 |

First cut was +78% stencils: the generic (untyped) families in every
location with cold runtime helpers. Now the generic `sub/mul/div`,
`test_rel/jrel/jnrel` and `===` stencils exist on memory only (a slot is
spilled first: they're the slow paths anyway), and one relation-mask
immediate plus one three-way runtime compare (`cloison_rt_compare`) replaces
four relations × two runtime calls. The stencil-name table is built by
`build.rs` (no startup hashing: ~-5% on short runs).

### Next for size: templates (proposal)

2335 stencils fall into 548 families that differ only in slot registers
(or a shift count): one template per family would cut stencil code ~67%
and the per-stencil tables (~130 B each) by ~1800 entries. arm64 already
instantiates register families (5-bit fields, uniform); extending its
fields to memory offsets and immediates (shift counts, condition codes)
is natural there. x86-64 only partially: register fields span REX bits
and REX presence changes instruction length, so only same-length
members (e.g. the rdi/rsi/rdx/rcx slots) can share a template.

---

## Group 2 (2026-10-07): natives, array length, `[]` in the JIT

No new opcodes: `Array` and `Object` (constructors, with `prototype` /
`constructor` links), `Array.prototype.pop`, `Function.prototype.call`
(a call through it to a bytecode function is unwrapped at the call site:
the interpreter's call slow path and `call_from_jit` call the receiver with
the window shifted, no nested interpreter).

| stencil | target | x86-64 as emitted | gap |
|---|---|---|---|
| `get_length_nc_<loc>` (`a.length`, receiver known to be an array) | `mov r12d, [rsi+32]` | ✓ | — |
| `get_length_<loc>` | cell check, `cmp byte [rsi+8], 1; jne exit`, load | `mov rax, r14; or rax, 2; test rsi, rax; jne; cmp byte; jne; mov` | the cell check's 3 instructions (shared pattern) |
| `create_array` (`[]`) | inline bump allocation (as `construct_direct`) | runtime call (`cloison_rt_create_array`, VM in `imm64`) | allocation not inlined yet |

Gaps found while running the new benchmarks (object-model work):
- `push` / `pop` are native calls through the runtime (`call`), not
  inlined stencils.
- Receivers seeing two array shapes (before and after the first `push`
  types the elements) make monomorphic caches miss; after 4 guard exits a
  property access is given up (`bail`): 69's `ps.push`, 44's `out.push`.
  Needs polymorphic caches, or guards on the shape's root for elements
  transitions.

---

## Strings (2026-10-07)

| stencil | target | x86-64 as emitted |
|---|---|---|
| `lda_constant` (a string or other non-double constant) | `movabs r12, imm64` | ✓ |
| `lda_constant_f` (a double) | `movabs rax, imm64; movq xmm0, rax` | ✓ |
| `get_length` on strings | as arrays' (same `elems_len` field; class 1 or 4) | ✓ (`cmp` of the class byte becomes a 2-value test) |
| `"v" + i` | inline concat into a fresh string | generic `add` → runtime (`generic_add` fast path: no intermediate buffers) |

The concatenation path is the gap: ~370 instructions per `"v" + i` (07),
mostly the runtime call wrapper, allocation and small `memmove` calls.
| `tid_jt/jf_<loc>`, `test_id_<loc>` (`==`/`===` of two non-string objects) | `cmp rsi, r12; je L` | ✓ |

---

## Object literals (2026-10-07)

| stencil | target | x86-64 as emitted | gap |
|---|---|---|---|
| `create_object_<n>` (n ≤ 8) | `movabs r11, heap; mov r12, [r11]; lea rax, [r12+8(6+n)]; cmp rax, [r11+8]; ja slow; mov [r11], rax; movabs rax, shape; mov [r12], rax; mov qword [r12+8], 0x100; xorps; movups ×2; mov qword [r12+48+8k], 10 ×n` | same, but `sub` + `cmp` for the bounds check (15 for n = 2) | +2 |
| `create_object` (n > 8) | a loop | LLVM's unrolled loop with remainder | larger, rare |
| `teq_*_<loc>` generic | equal bits / nullish / two non-string cells decided inline | ✓ | — |

---

## Property misses (2026-10-07)

| case | target | as emitted |
|---|---|---|
| `p.missing` (absent everywhere) | `undefined` constant behind the receiver's shape guard (and the epoch check of code) | `get_proto_nc` load of the `missing_holder` slot: one load ✓ (a constant would save the load, and let `=== undefined` fold) |

---

## `switch` (2026-10-07)

| stencil | target | x86-64 as emitted | gap |
|---|---|---|---|
| `jseq_smi_i` (a case on an int32) | `cmp r12d, k; je L` | ✓ (asm, as `jlt_smi_i`) | — |
| a dense `switch` | `cmp eax, n; ja default; jmp [table + rax*8]` | a chain of `jseq_smi_i` | a jump table (needs a table of patched addresses in JIT memory) |

---

## `for … in` (2026-10-07)

| piece | target | as emitted | gap |
|---|---|---|---|
| `for_in_keys` | a cached enumeration per shape (V8's enum cache) | runtime call, building a fresh array of strings each time | the cache |
| the loop | index compare and keyed load on a known array of strings | `get_length` + `jgt` + `get_keyed` (generic stencils for the keys' array) | typed keys array |

---

## Polymorphic named access (2026-10-07)

2-way load, receiver in rsi, cases S1 (off1), S2 (off2):

| piece | target | x86-64 as emitted | gap |
|---|---|---|---|
| cell check | `mov rax, r14; or rax, 2; test rsi, rax; jne exit` | ✓ (`guard_cell`) | 2 insns with a pinned not-cell mask (as noted for every guard) |
| dispatch | `mov rax, [rsi]; movabs rcx, S1; cmp rax, rcx; je B1; movabs rcx, S2; cmp rax, rcx; jne exit` | `movabs rax, S; cmp [rsi], rax; je/jne` per case | the shape reloaded per compare (an L1 hit; one load saved per extra case) |
| bodies | `B2: mov r12, [rsi+off2]; jmp D; B1: mov r12, [rsi+off1]; D:` | ✓ (`_nc` accesses; identical ones shared) | — |
| consecutive accesses on one receiver | one dispatch, then specialized code per shape | a dispatch per access unless the shapes extend one another | splitting the path by shape |

## Polymorphic inlined call (2026-10-07)

| piece | target | x86-64 as emitted | gap |
|---|---|---|---|
| callee dispatch | `movabs rax, F1; cmp rdi, rax; jne next` per case | ✓ (`callee_jne`) | — |
| argument moves | once, before the dispatch | ✓ (hoisted) | — |
| receiver in the case | known shape (the method picks it) | ✓ unguarded (`assume`) | — |
| method load + callee dispatch | one shape dispatch: `guard_cell; movabs; cmp [r], rax; jne next` per case | ✓ when the method's register is dead after the call | — |

---

## New stencils, ideal code pass (2026-10-08)

Before → after, x86-64 instructions on the hot path (LLVM 23.1.3).

### Inlined `new` (was `alloc_this`, 49)

| piece | target | as emitted | gap |
|---|---|---|---|
| prototype check | `movabs rax, &F.prototype; movabs r10, P; cmp [rax], r10; jne exit` | ✓ `guard_word` (4) | — |
| capacity check | none: capacity only grows after settling; old-shape objects stay valid | ✓ dropped | — |
| allocation + header | as `create_object_<n>`: bump, `cmp`, `ja slow`, shape, `0x100`, 4 zero words | ✓ `create_object_<n>` (15) | +2 (as object literals: `sub` + `cmp`) |
| scalar-replaced (`ctor_check`, was 12) | the prototype check, `mov r12d, 0xa` | ✓ (6) | — |

### `a.push(v)` (was `array_push`, 46, plus window spills)

Loop in 40, per push: target ~18, was ~60, now 33.

| piece | target | as emitted | gap |
|---|---|---|---|
| method load | none (only the receiver's shape dispatch is needed) | ✓ check only (`drop_callee_load`; exits store the callee, `store_const_m`) | — |
| callee check | none (prototype slot proven) | ✓ dropped (`proven_callee` through `Prop::Poly`) | — |
| argument | in `acc` | ✓ (`Ldar` skipped after the setup's `Star`) | window copies (`mov r9`, boxed store) kept for exits |
| receiver checks | none after the dispatch (`Ext` of an array shape) | ✓ `_nc` | — |
| fits (int32) | `mov rax, [rdi]; cmp qword [rax], 2; ja slow` | ✓ `_i` | — |
| store + length | `mov eax,[rdi+32]; cmp eax,[rdi+36]; jae slow; mov r11,[rdi+24]; mov [r11+rax*8], v; inc; mov [rdi+32]; mov r12d` | ✓ | re-box of the int (`mov r10d, r12d; or r10, r14`) though r12 was boxed by the window store |
| slow path | out of line | ✓ `stencil_slow` | — |

`array_push_i_nc_g0` 13, `array_push_nc_g0` 28 (generic value), `array_push_g0` 32.
`array_pop_nc_g0` 14 (was 23 + spills), `array_pop_g0` 20.

### `o[k] = v` (was `set_keyed_<loc>`, 37)

| variant | target | as emitted |
|---|---|---|
| `nc_i` (array known, int32 value) | key load + int check (3), bounds (2), `cmp qword [code], 2; ja` (3), store (2) | ✓ 16 |
| `nc_o` (an object, `Obj` or `Ext`) | as `nc_i`, fits `code == 0 \|\| code == [v]` | ✓ |
| `nc` (generic value) | 4-way dispatch on the code | 31 (≈19 executed on the shape path) |

Gaps: the key is spilled from its int slot and reloaded (3 + 3); per-key-slot
variants would cost ~126 stencils. 43 and 84 now use `nc_o`.

### Interpreter: polymorphic named access

| piece | target | was | now |
|---|---|---|---|
| way 0 miss → `poly_get_named` hit at way k | `cmp [o], shape_k` per way, load `loc`, load slot, dispatch (~15) | ~45: hit swapped to way 0 through the stack (4 × 16-byte loads/stores + spills) | ~18, no swap |

Interpreter-only cycles: 25 −12%, 26 −18%, 27 −29%; 80, 81 +1%.

## Inlined calls, ideal code pass (2026-10-08)

Benchmarks where cloison's lead over V8 was narrowest were call-heavy (15 1.12×,
50 1.16×, 12 1.20×, 10 1.29×). Per-iteration x86-64 instructions, before →
after, and the hand-written target.

### Argument passing into an inlined callee

| piece | target | was | now |
|---|---|---|---|
| `this` / argument the callee only reads | nothing: the callee reads the caller's register | `Ldar w; Star p` per parameter, `p` mostly a memory home: 2–4 insns + memory reads in the body | ✓ aliased (`arg_moves`, `read_only`): exits copy `w` into `p` (`materialize`) |
| argument the caller copied into its window (`Mov x, w` / `Ldar x; Star w`) | read `x` | callee read `w` (memory) | ✓ reads `x` (`window_sources`); `w` still stored, for exits at the call |
| missing argument / `this` of `CallUndefined` | nothing if unread | `mov r12d, 0xa; mov [p], r12` | same (could be an exit constant) |

12 (`Level.weakestOf(a, b)` → `this.weaker(a, b)`): 95 → 65 insns/iter,
9.4M → 7.4M cycles. 15: 85 → 70. 10: 43 → 38.

### What's left (12, 15, 14)

| gap | target | as emitted |
|---|---|---|
| short-lived temporaries of inlined frames (window copies, `t = this.x` before `+=`) | a free GPR | memory home: `mov [r13+k], r12` / `mov eax, [r13+k]` (homes are whole-function, 6 GPR slots, by static use count) |
| `Sub.parent.prototype.mark.call` (15), `Leaf.parent.call` (14) | nothing in the loop: constants (V8: const field tracking + code dependencies) | was ~45 (global guard + 3 × tag test, shape guard, load + callee check); now `guard_word` + `lda_constant` per link (5), none for a prototype's slot, callee check gone (`fold_constant_loads`): ~20. Left: the checks are loop-invariant (no LICM) and the constants are still stored to their registers |
| callee loaded from an own property, checked, register dead (`this.weaker`) | `cmp [obj+off], imm; jne` | load, store to memory home, `cmp [mem], rax` |
| window copy kept only for exits at the call's guard | none (exit-time copy) | `mov [r13+w], r12` per argument |

Executable memory: a recompile mapped onto the page its previous version
was just freed from ran 46M or 61M cycles (13, frontend stalls 1.2M vs
8.8M, same code); code now comes from a reserved range never reused.
Loop-head alignment (16/32/64 with `nop` padding): ±5% either way, dropped.

Results vs 57628c5: 50 −22%, 14 −22%, 12 −21%, 10 −21%, 64 −19%, 15 −16%.

## Call setup and register slots (2026-10-08)

| piece | target | was | now |
|---|---|---|---|
| inlined method call `o.m(x)` (proven callee) | receiver check if needed, body | `get_proto` + `star` (method), `ldar o; star w` (window copy), `lda_undefined` | the body: setup only exits read is dropped (`drop_setup`, restores at exits) |
| short-lived temporaries of inlined frames | a free GPR | memory home | shared slots (`interference`): 82/84 34% of stencils on memory (was 48%), 63/80 17% (was 30-44%) |
| `none++` in a branch (02) | in-slot `inc` | `addi_uK` | `ldar_m inc_i star_m_i`: loop weighting favours temporaries with more uses |

Sharing in steady state: 80 ×20: −1.6% instructions, −1% cycles. Memory
homes cost less than counted (L1 + store forwarding).

## Loop-invariant checks (2026-10-08)

30, per iteration (x86-64):

| piece | target | was | now |
|---|---|---|---|
| `d == Dir.FORWARD` | `cmp esi, 1; jne` (Dir constant, checked before the loop) | global slot check (4) + `lda_constant` + `star` + `get_own` (tag, shape, load: 9) + compare: ~17 | `mov r12d, 1` + compare (`lda_smi_i teq_jf_ii`): 3; the checks before the loop (`hoist_guards`), the store of `Dir` gone (dead) |

51M → 25M instructions, 8.7M → 5.4M cycles. Left: `lda_smi_i; teq_jf_ii_g1` could be `cmp reg, imm` (fusion of `LdaSmi; TestEqual r; JumpIf*`).

## Direct self calls with extending shapes, compile time (2026-10-08)

68's `sum(n.kids[i])`: target `call` straight into `sum`'s code; was
`cloison_rt_call` → `vm_run` → entry (the argument `Ext(S)` didn't fit the
entry's `Obj(S)`); now `call_direct` (entry types `Ext` for `this` and
parameters). 68: 26.5M → 13.6M cycles. Left in 68: `[]` and the first
push through the runtime (`cloison_rt_create_array`, `array_push`,
`elements_transition`: ~35 cycles per array; `CreateArray` has no cache
slot to remember what its arrays become).

Compile time is now the largest gap in 80/82/91 (half the run).

## Group 3 (2026-10-09): bitwise on registers, `~x`, `-x`, ToNumber, typed arrays, upvalues, keyed runtime paths

Opcodes from 75d2217, 4ad23f9 and cb6b81b. Ideal first, then the stencils
as emitted (LLVM 23.1.3, `scripts/stencils-asm.sh`), then the JIT's
sequences in Octane (`CLOISON_JIT_LOG`, `CLOISON_COUNT`, `perf` with
`CLOISON_PERF_MAP`). Hot-path instructions, excluding the trailing `jmp`.

### Bitwise on two registers: `r & acc`, `r | acc`, `r ^ acc`, `r << acc`, `r >> acc`, `r >>> acc`

int32 both, `a` in an int32 slot:

    x86-64:  and r12d, esi          (or, xor)
             shlx r12d, esi, r12d   (sarx, shrx: BMI2; count mod 32 in hardware)
             without BMI2: mov ecx, r12d; mov r12d, esi; shl r12d, cl  (rcx is slot 3: saved around it)
    arm64:   and w20, w25, w20 ; lsl w20, w25, w20 (asr, lsr; count mod 32 in hardware)
    >>>:     shrx eax, esi, r12d ; test eax, eax ; js exit ; mov r12d, eax
             lsr w16, w25, w20 ; tbnz w16, #31, exit ; mov w20, w16

| stencil | x86-64 as emitted | arm64 | gap |
|---|---|---|---|
| `bit_and/or/xor_ii_<loc>` | `and r12d, edi` ✓ (`_m`: `and r12d, [r13+k]` ✓) | `and w20, w20, w25` ✓ | — |
| `shl/sar_ii_<loc>` | `mov rax, rcx; mov rcx, r12; mov r12d, esi; shl r12d, cl; mov rcx, rax` (5) | `lsl w20, w25, w20` ✓ | x86-64: +4 (rcx is a slot); `shlx` needs BMI2 (target-cpu, or a runtime-selected stencil set) |
| `shr_ii_<loc>` | as above + `test; js` (8) | `lsr; tbnz; mov` ✓ | x86-64: +4 |
| `shr_iif_<loc>` (`>>>` past int32) | rcx dance + `cvtsi2sd xmm0, r10` (64-bit source: exact) | `lsr; ucvtf d0, w8` ✓ | x86-64: +4 |
| generic `_m` (`bit_and_m`, …) | both-int32 test, op, box; `__slow` ToInt32s | similar | — |

Rare in Octane (gbemu `bit_or_ii` 0.3%): left.

### `~x` (BitNot), `-x` (Negate)

    ~ int32:   not r12d                         | mvn w20, w20
    - int32:   test r12d, 0x7fffffff ; jz exit ; neg r12d     (0 → -0, INT32_MIN → 2^31: exit)
               tst w20, #0x7fffffff ; b.eq exit ; neg w20, w20
    - double:  movabs rax, 1<<63 ; movq xmm8, rax ; xorpd xmm0, xmm8    (no .rodata: mask built)
               fneg d0, d0
    - int32 → double (after the exit): xorps; cvtsi2sd xmm0, r12d; then as above | scvtf; fneg

| stencil | x86-64 | arm64 | gap |
|---|---|---|---|
| `bit_not_i` | `not r12d` ✓ | `mvn` ✓ | — |
| `bit_not` (generic) | `cmp r12, r14; jb slow; not r12d; or r12, r14` ✓ | ✓ | — |
| `neg_i` | `test; jne L; jmp exit; L: neg` (exit inline, a taken branch) | same shape | +1 and a taken branch: LLVM puts the exit on the fall-through |
| `neg_f` | `movabs; movq; xorpd` ✓ (asm, as target) | `fneg` ✓ | — |
| `neg_iif` | `xorps; cvtsi2sd; movabs; movq; xorpd` ✓ | `scvtf; fneg` ✓ | — |

### ToNumber (`x++`'s old value)

    acc known int32/double: nothing ✓ (`nop`)
    otherwise:  test r12, r14 ; jz exit           | tst x20, x22 ; b.eq exit

`check_acc_num` as emitted: `test r14, r12; jne continue; jmp exit` (both
arches): +1 and the hot path is the taken branch, as `neg_i`. Rare (only
where the type isn't known): left.

### Typed arrays: `o[i]` (`tget_<w>_<loc>`), `o[k] = acc` (`tset_<case>_<loc>`)

`o` in rsi / x25, `i` in acc, a known kind (the receiver's shape says so):

    load u16:  cmp [rsi+32], r12d ; jbe exit ; mov rax, [rsi+24] ; mov r10d, r12d ; movzx r12d, word [rax+r10*2]
               ldr w8, [x25,#32] ; cmp w8, w20 ; b.ls exit ; ldr x8, [x25,#24] ; ldrh w20, [x8, w20, uxtw #1]
    (`mov r10d, r12d`: acc's high half may be the tag)
    store (key k in an int32 slot edi, value int32 in acc):
               cmp [rsi+32], edi ; jbe exit ; mov rax, [rsi+24] ; mov [rax+rdi*2], r12w
               ldr w8, [x25,#32] ; cmp w8, w24 ; b.ls exit ; ldr x8, [x25,#24] ; strh w20, [x8, w24, uxtw #1]
    kind check (`_chk`, receiver type unknown): cell test, then class 7 at byte 8 and kind at byte 10:
               mov rax, r14 ; or rax, 2 ; test rsi, rax ; jne exit
               mov eax, [rsi+8] ; and eax, 0xff00ff ; cmp eax, 7 | k<<16 ; jne exit

| stencil | x86-64 as emitted | arm64 | gap |
|---|---|---|---|
| `tget_<w>_<loc>` | ✓ (5) | `ubfiz` + `ldrh [x8, x9]` (6) | arm64: +1 (`uxtw #1` addressing not used) |
| `tget_<w>_chk_<loc>` | `movabs` kind; cell (4); `cmp byte [rsi+8], 7; jne`; `movzx; cmp; jne`; then as above (15) | 4 × `movz/movk` kind, … (19) | +2 (masked dword compare, kind in a 32-bit immediate) |
| `tset_<case>_<loc>` | was `mov eax, imm; cdqe; mov rax, [r13+rax]` for the key; now `mov rax, [r13+k]` (1): 7 | `movz; ldr [x21, x8]` (was `movz; movk; ldr sxtw`) | the key: boxed, spilled from its slot, reloaded, int-checked: +5 over the slot form (below) |
| `tset_b8_f` etc. (double into an int kind) | exponent check, `cvttsd2si` | similar | — |

**The global heap views** were the biggest gap: Mandreel reads
`HEAPU16[i >> 1]`, `HEAP32[…]`, … in every function, each a global past
the global object's in-object slots, so every access was `lda_global_oop`
(shape check, `props` load, slot load: 9) + `tget_u16_chk` (15). Target:
the view a constant, its kind known, `tget_u16` alone. `fold_constant_loads`
skipped them twice: out-of-line slots (`oop`) are never folded, even when
the field is constant and no check (whose address would move) is needed;
and any `SetKeyed` in the function disabled constant fields, though only
`set_keyed_rt` can write a named field of an arbitrary object (the
generic `set_keyed_*` stencils write array elements, or go through the
runtime only for a typed-array receiver). Now: a constant global in an
out-of-line slot folds without a check, `SetKeyed` other than
`set_keyed_rt` only blocks constant fields of typed arrays, and a folded
typed array types `acc` as `Ext(shape)` (its kind never changes), so its
accesses lose `_chk`. Mandreel: `lda_global_oop` 544M → 42M executions,
`tget_u16_chk` 312M → 13M (`tget_u16` 299M), hot JIT code −35%.

**Interpreter**, same opcodes: `slow_set_keyed` stored every number
through `to_number` and `TypedKind::store`'s `to_int32`, which was
`trunc().rem_euclid(2^32)` (an `fmod` call) for every double. Now an
int32 is stored as is (`TypedKind::store_i32`), and `to_int32` of a
double below 2^63 in magnitude is the truncation's low 32 bits
(`cvttsd2si`), `fmod` only past that; this is also `cloison_rt_to_int32`
(navier-stokes 2.9%).

Left: the key of `tset` from its slot directly (the ideal store above):
today `star_g<k>_i` boxes it, a spill stores it, `tset` reloads and checks
it (≈6 instructions per store, Mandreel ~300M stores). Needs the key's
slot in the stencil name: 11 cases × 7 receiver locations × 12 key slots,
or only the hot cases (`b8_i`, `b16_i`, `b32_i`, `f32_f`, `f64_f` with the
receiver in a GPR: ~330 stencils, ~40 KB with tables) — or templates
(arm64). gbemu's `_chk` (`this.memory`, a field that first held `[]`, so
of type `Any`): the masked compare would save 2 of ~15.

### Upvalues: `lda_upvalue` / `sta_upvalue`, `_c`, `set_frame_func`

The frame's function is in the header (`[regs - FUNC_OFF]`, 80); its cells
follow its in-object slots, as many as the function shape's capacity,
the same for every function object with upvalues; a cell points to the
variable.

    lda_upvalue k:  mov rax, [r13-80] ; mov rax, [rax + 48+8·cap+8·k] ; mov rax, [rax] ; mov r12, [rax]
                    ldur x8, [x21,#-80] ; ldr x8, [x8,#off] ; ldr x8, [x8] ; ldr x20, [x8]
    _c (one closure: the cell a constant):  movabs rax, cell ; mov rax, [rax] ; mov r12, [rax]
    set_frame_func: movabs rax, F ; mov [r13+off], rax  |  movz/movk ×4 ; str x8, [x21,#off]

| stencil | was | now (x86-64) | arm64 now | gap |
|---|---|---|---|---|
| `lda/sta_upvalue` | `movabs` FUNC_OFF; `sub`; shape load; `movabs` capacity offset; capacity load; `lea`; `mov; movsxd` k; 3 loads (12) | ✓ 4 loads (offset computed by the JIT from `function_shape`'s capacity; `FUNC_OFF` a constant) | `ldur; movz; ldr [x8, x9]; ldr; ldr` (5, was 20) | arm64: +1 (`movz` for the offset; a scaled `ldr` immediate would need a new relocation) |
| `lda/sta_upvalue_c` | `movabs; mov; mov` ✓ | ✓ | `movz/movk ×4; ldr; ldr` ✓ | — |
| `set_frame_func` | `movabs; mov r10d, imm; movsxd; mov [r13+r10]` (4) | same | `movz/movk ×4; movz; movk; str sxtw` (7) | +2 / +2: the offset can be negative (`8·frame − 80`), which `at_imm` doesn't take on arm64; once per call to a function with upvalues: left |

navier-stokes: `lda_upvalue` was 3.2% of cycles.

### Keyed runtime paths: `get_keyed_rt_<loc>`, `set_keyed_rt[_i]_<loc>`

A site the fast stencils kept leaving (an object that isn't an array with
integer keys: crypto's `BigInteger`, `this[i] = …`). Ideal: the
interpreter's own fast path inline, the runtime only for the rest:

    store, own element in bounds, elements' type fits (int32 value: code ≤ 2):
        cell (4) ; mov rax, [r13+k] ; cmp rax, r14 ; jb slow        (key int32)
        mov r10, [rsi] ; cmp qword [r10], 2 ; ja slow                (Shape::elems_code)
        cmp [rsi+32], eax ; jbe slow ; mov r10d, r12d ; or r10, r14 ; mov r11, [rsi+24] ; mov [r11+rax*8], r10

| stencil | was | now | gap |
|---|---|---|---|
| `get_keyed_rt_<loc>` | own element inline (class 0–3, 5; not a hole), else `cloison_rt_get_keyed` | same | the class test is two compares (`cmp 4; jb; cmp 5; jne`): +1 |
| `set_keyed_rt_i_<loc>` | always `cloison_rt_set_keyed` → `Vm::set_keyed` → `property_key`, `store_element` (~5.7% of crypto) | own element inline, as `op_set_keyed` (`own_element_store`): 17 on x86-64, 19 on arm64; the runtime in `__slow` | cell test as `test rsi, r14; jne; test sil, 2; jne` (4, no mask register) |
| `set_keyed_rt_<loc>` (boxed value) | same | inline, a 4-way fit test on the code | — |

(The slow helper passes `NULL` as its unused second argument: LLVM merged
the identical `set_keyed_` helper into it otherwise, and a stencil can't
jump into another's helper.)

### Results (x86-64, cycles:u, interleaved base/new runs, median of paired ratios; vs 9a32511)

| | base | new | change | spread |
|---|---:|---:|---:|---|
| DeltaBlue (`notes/deltablue/octane-db.js`) | 3462M | 3465M | +0.3% | −0.3..+1.2% (noise) |
| DeltaBlue (Octane driver) | 3469M | 3468M | −0.1% | −0.7..+0.4% (noise) |
| mandreel | 16667M | 13409M | **−19.3%** | −19.7..−18.9% |
| crypto | 23456M | 21972M | **−5.9%** | −7.3..−5.5% |
| gbemu | 4548M | 4398M | **−3.3%** | −3.9..−2.7% |
| navier-stokes | 26223M | 25893M | **−1.2%** | −1.4..−1.2% |

By step (each against the one before): constant heap views −14.1%
mandreel; `set_keyed_rt` fast path −6.5% crypto; `lda_upvalue` −1.6%
navier-stokes; `to_int32` / `store_i32` −4.4% mandreel, −2.7% gbemu,
−0.8% navier-stokes. DeltaBlue uses none of these paths.

Sizes: runtime .text +0.2% (1 869 161 → 1 872 489), .rodata +0.3%,
stencils.text +0.2% (101 880 → 102 112); JIT code for the bytecode
listings unchanged. JIT code in Octane (hot / all, bytes): mandreel
944 921 / 1 675 880 → 613 418 / 1 263 072 (−35% / −25%), navier-stokes
166 331 → 144 829 hot (−13%), crypto and gbemu ±1%.

Tests: `global_typed_arrays_agree_with_node` (out-of-line constant
globals, then replaced by another kind), `own_element_stores_agree_with_node`
(`set_keyed_rt` with values that fit, then don't),
`to_int32_agrees_with_node`.

Left, biggest first: `tset`'s key from its slot (Mandreel, ~6 per store);
the interpreter in Mandreel and gbemu (`op_lda_global` 6%, `vm_run` /
`cloison_rt_call` 21% of gbemu: not these opcodes); `_chk`'s masked kind
compare (gbemu, 2 per access); shifts on registers via BMI2 (x86-64, 4);
`neg_i` / `check_acc_num` exit layout (1); arm64's register-offset loads
(`lda_upvalue`, `tget`: 1 each); `set_frame_func`'s offset (2).

Micros (`scripts/perf.sh --ab` vs 9a32511, 76 benchmarks): none changed
beyond its spread.

## Group 3, second pass (2026-10-09): the keyed store's key, `_chk`, exit layout

### The key in a scratch register (`key_*`)

`tset_*` and the specialized `set_keyed_{i,o,nc,nc_i,nc_o}_*` read their
key from r11 / x15, zero-extended, which a `key_*` piece just before sets:
`key_u<k>` (`mov r11d, esi`), `key_g<k>` (`cmp rsi, r14; jb exit; mov r11d,
esi`), `key_m` (load, check, `mov`). One stencil per receiver location, no
variant per key slot (that would have been ~360 stencils on x86-64, ~1440
on arm64); 25 new pieces (13 on x86-64). Every stencil reading the key
reads it before anything writes r11 / x15: checked on the disassembly of
both (`key_x` is an asm with side effects, first in the stencil). The
generic `set_keyed_<loc>` keeps its key in memory: its `__slow` helper
(typed arrays, through the runtime) couldn't count on r11 surviving.
`CLOISON_COUNT`'s x86-64 counter used r11 (`movabs r11; inc [r11]`): now rax.

| piece | was (x86-64) | now | target |
|---|---|---|---|
| `tset_b16_i_g0` + key in boxed slot g5 | `spill_g5` (1) + `mov rax,[r13+k]; cmp; jb` (3) + bounds, store (5) = 9 | `key_g5` (3) + `cmp [rdi+32], r11d; jbe; mov rax,[rdi+24]; mov [rax+r11*2], r12w` (4) = 7 | 4 with the key in an int32 slot (`key_u`: 1 + 4) |
| arm64 same | `movz; ldr`; check; … (9) | `key_g` (3) + `ldr; cmp; b.ls; ldr; strh [x8, x15, lsl #1]` (5) | ✓ |
| `set_keyed_nc_i_m` + key in g0 (crypto) | `spill_g0` + 10 | `key_g0` (3) + 8 | — |

Mandreel −3.4% (the `CLOISON_COUNT` bug made the first count look like a
regression: every key clobbered, every store exiting, the sites given up
to `set_keyed_rt`). crypto unchanged: its keys sit in boxed slots, so the
int check only moved. Left: keys in boxed slots (`star_g5_i` boxes, `key_g`
checks): slot homes, not these stencils.

### `_chk`: class and kind in one compare

`*(u32 *)(o + 8) & 0xff00ff == 7 | kind << 16` (`typed_class_kind`), the
expected word in the 32-bit `imm` (free in both now): x86-64 `mov eax,
0xff00ff; and eax, [rsi+8]; mov r10d, imm; cmp eax, r10d; jne` (5, was 6
with a `movabs`), arm64 `movz; movk; ldr; and; cmp; b.ne` (6, was 10).
x86-64 target 4 (`cmp eax, imm32`: LLVM materializes the hole). gbemu
−0.7% (within its spread).

### Exits off the fall-through (`core::hint::cold_path` in `exit!`)

`neg_i`, `key_g`, `get_keyed_*`, `set_keyed_*`, `construct_direct` and the
`mod_smi` helpers (52 of 2725 stencils on x86-64) had `jcc ok; jmp exit;
ok:`: the hot path took a branch. With the hint the exit moves to a stub
at the end. **Correction (third pass):** the DeltaBlue −3.2% / −2.4%
measured here was alignment, not the hint: both builds run exactly the
same instructions and branches on DeltaBlue (17 645M / 3 632M), and
shifting the same build's code by a few bytes moves DeltaBlue by 4.4%
(below). And the stub isn't the target either: LLVM reaches it with a
short `jcc`, so the hot path's final `jmp cloison_continue` is no longer last
and can't be trimmed (one taken jump either way). (`check_acc_num` was
already ideal: `build.rs`'s `swap_x86_final_branch` makes it `test; je
exit`; the analyses above that used `scripts/stencils-asm.sh` read the
raw object, before `build.rs`'s rewrites.)

### Still left

Shifts on registers (x86-64: rcx is slot 3; `shlx` needs BMI2 dispatch);
`imm` folded into compares on x86-64 (`_chk`, 1); arm64's
`ubfiz` + register-offset loads (`tget_u16`, `lda_upvalue`: 1 each);
`set_frame_func`'s offset (2, once per call).

### Results, second pass (x86-64, cycles:u, interleaved, 9 rounds; vs d1f52fa)

| | change | spread |
|---|---:|---|
| DeltaBlue (`octane-db.js`) | **−2.5%** | −3.5..−2.4% |
| DeltaBlue (Octane driver) | **−3.0%** | −3.6..−2.7% |
| mandreel | **−2.7%** | −2.8..−2.5% |
| navier-stokes | **−1.8%** | −1.9..−1.7% |
| gbemu | +0.3% | +0.3..+0.5% |
| crypto | +0.5% | +0.4..+0.7% |

Micros (`perf.sh --ab`, 76): none slower; 84-build-and-tear-down −10.0%
(−7.4% instructions), 16-inner-function −1.8%. Sizes: stencils.text
102 112 → 97 171 (−4.8%: no key load in each store stencil), runtime
.text +0.1%, .rodata −0.8%; JIT code for the listings unchanged. Tests
pass on x86-64 and arm64 (qemu).


## Group 3, third pass (2026-10-10): stubs, and measuring DeltaBlue

### Exits through stubs (`-mc-relax-all` where it pays)

Of 3053 x86-64 stencils (as the JIT copies them, after `build.rs`), 516
had an unconditional `jmp cloison_continue` that wasn't their last
instruction: an exit or `__slow` stub followed, reached by a short `jcc`
that `fold_x86_stubs` (no decoder: long jumps only) couldn't retarget.
Each execution then took the `jmp` to the next piece. Hottest: `inc`
(navier-stokes 857M), `set_keyed_nc_i_m` (crypto 628M), `set_keyed_nc_g0`
(252M), `mul_ii` (crypto 12% of cycles), `sub_m`, `add_<loc>`.

Target, e.g. `inc`: `cmp r12, r14; jb slow; mov eax, r12d; inc eax; jo
slow; mov r12d, eax; or r12, r14` and fall through. Now:

- `build.rs` compiles the stencils a second time with every branch long
  (`-Cllvm-args=-mc-relax-all`), where the stubs fold; a stencil takes
  that code where it ends in `jmp cloison_continue` and is no longer than the
  normal one (all of them: 3053 stencils, 102 058 → 101 818 bytes; was
  +4% when every stencil took it, and `teq_jt` grew 147 → 158 bytes).
  516 → 239 stencils that can't trim their last jump.
- `fold_x86_stubs` now folds a stub with several long users, and never
  one that the instruction before can fall into (it requires `jmp rel32`
  to a symbol there): `jtofalse`'s `jne continue; jmp jump_target` lost
  its last instruction without that (wrong results in the first try).
- `key_*`, `neg_i` and `mul_ii` exit through `asm goto` with long `jcc`s
  (`exit_unless_int32`, `mul_ii_fast`), so they're ideal in either form.

| | branches before → after | cycles (A/B) |
|---|---|---|
| crypto | 28 027M → 24 106M (−14%) | −2.2% median over 16 code shifts (all −1.2..−3.5%) |
| navier-stokes | 20 241M → 18 911M (−6.6%) | +0.2% (noise) |
| mandreel | 9 072M → 8 730M (−3.8%) | +0.2% |
| gbemu | 4 062M → 3 914M (−3.6%) | +0.1% |
| DeltaBlue | 3 632M → 3 632M | −0.1% mean over 16 shifts |

### Measuring DeltaBlue: alignment

DeltaBlue's cycles move with where the hot code sits, not only with what
it runs. HEAD's build, every piece shifted by `CLOISON_CODE_SHIFT` bytes:
3361M (0), 3483M (8), 3467M (16), 3379M (24), 3411M (32), 3350M (40),
3498M (48), 3448M (56): 4.4%. Any stencil changing size moves the code
after it, so an A/B of two builds can show ±3% on DeltaBlue with the same
instructions (this pass's first try: +2.9%, all of it alignment). Compare
DeltaBlue by instruction and branch counts, and cycles as the median over
`CLOISON_CODE_SHIFT=0,4,…,60` (each the best of 2 runs, interleaved). L1i
misses also flip between ~0.7M and ~1.9M per run on either build (the
host is shared; sibling hyperthreads), worth ~80M cycles.

Micros (`perf.sh --ab`, 76): none slower; 46-polymorphic-elements-scan
−1.9%, 67-arg-to-external-method −1.7%. Sizes: stencils.text (the normal
object) +0.2%, the table the JIT copies 102 058 → 101 818 bytes; runtime
flat. Tests pass on x86-64 and arm64 (qemu).

## Group 3, fourth pass (2026-10-10)

| gap | was | now |
|---|---|---|
| `set_keyed_nc` (array of known shape, generic value; navier-stokes 252M) | load `elems_code`, 4-way dispatch whose arms rejoin the store, final `jmp` not trimmable (89 B) | the exact shape fixes the elements' type at compile time: `nc_a` (any: bounds, store; 23 B), `nc_n` (numbers: `test r14, r12; je exit` + that; 32 B), `nc_ia` (int32 into int/num/any: bounds, box, store); an `Ext` receiver keeps the dispatch |
| `sar_smi` / `shl_smi` on `acc` of no known type (crypto 107M) | generic: tag test, `mov rax, rcx; mov ecx, k; sar r12d, cl; or r12, r14; mov rcx, rax` + stub | `check_acc_int` (`cmp r12, r14; jb exit`) + `sar_smi_i_<k>` (`sar r12d, k`); a site whose check exits is widened to the generic stencil |
| arm64 typed-array element address | `ubfiz x9, x20, #1, #32; ldrh w20, [x8, x9]` | `ldrh w20, [x8, w20, uxtw #1]` (indexed through a pointer of the element's size, `elem_at`) |

Instructions / branches (millions, x86-64): crypto 129 437 / 24 110 →
128 120 / 23 481 (−1.0% / −2.6%); navier-stokes 95 225 / 18 911 → 92 787
/ 17 828 (−2.6% / −5.7%); mandreel, gbemu, DeltaBlue unchanged. Cycles
over 8 code shifts: crypto −0.6% (−1.0..+0.6), navier-stokes +0.7%
(−0.2..+1.0): within the shift spread. Micros: none changed. Sizes:
stencils.text +1.1% (21 new stencils), runtime flat. Test:
`untyped_shifts_agree_with_node`.

### Left (small)

- arm64 field and upvalue loads take their offset from a `movz` (`ldr x,
  [b, x9]`); a `LDST64_ABS_LO12_NC` hole would make them one `ldr x, [b,
  #off]` but needs a variant per stencil for offsets ≥ 4096 (the JIT
  patches lo12 without an overflow check), and arm64 isn't measurable
  here.
- Shifts by a register on x86-64: rcx is slot 3 (+3 moves); `shlx` needs
  BMI2 dispatch. 1–10M executions per benchmark.
- `set_frame_func`'s offset (+2, once per call to a function with
  upvalues: ~20K in navier-stokes).
- 239 stencils still end in a jump to a stub (their long-branch form
  would be longer); the hottest left are `jtofalse` (DeltaBlue 28M),
  `teq_jt_g*` (19M), `array_push_nc_*` (18M), `call` (crypto 14M).

---

## T1 (2026-10-14): Navier-Stokes `lin_solve`'s inner loop, at f6d86a0

    for (var i=1; i<=width; i++)
        lastX = x[currentRow] = (x0[currentRow] + a*(lastX+x[++currentRow]+x[++lastRow]+x[++nextRow])) * invC;

Ideal (x86-64), with `x`/`x0` element bases and lengths checked once
before the loop (`currentRow+1 .. currentRow+width+1` within both), the
indexes unboxed in GPRs, `lastX`, `a` (as a double), `invC` in XMMs, the
elements known doubles (or decoded branch-free):

    loop: movsd  xmm1, [x0 + cr*8]          ; x0[cr]   (decode: 2 ops if boxed)
          movsd  xmm2, [x + cr*8 + 8]       ; x[++cr]
          addsd  xmm2, xmm0                 ; lastX + …
          addsd  xmm2, [x + lr*8 + 8]       ; x[++lr]
          addsd  xmm2, [x + nr*8 + 8]       ; x[++nr]
          mulsd  xmm2, xmm_a
          addsd  xmm1, xmm2
          mulsd  xmm1, xmm_invC
          movsd  [x + cr*8], xmm1 ; movaps xmm0, xmm1
          inc cr ; inc lr ; inc nr ; inc i ; cmp i, width ; jle loop

About 20 instructions (35 with boxed elements decoded). The JIT emits
about 120 (c2 of `CLOISON_JIT_DUMP=…%n CLOISON_JIT_DUMP_FN=lin_solve`):

| part | now | ideal | why |
|---|---:|---:|---|
| 5 element loads | ~55 | 15–20 | each: bounds check (2), load (2), hole check (2), int-or-double decode (5, branchy: the arrays start as int 0s) |
| partial sums | ~16 | 0 | the bytecode's temporaries (`add_ff_g2 star_g2_f`) are homed per register over the whole region; a temporary reused for an array or an index joins to Any, so a GPR boxed: each sum boxed (`movq; sub`) and unboxed (`lea; movq`) again |
| indexes | ~15 | 3 | `add ecx,1; jo` per increment; `nextRow` (7th live int) in memory, boxed back on each increment |
| `a` | 3 | 0 | int parameter in memory, `cvtsi2sd` every iteration |
| loop control | ~8 | 3 | `width` upvalue reloaded and checked each test |

Fixes, by size of the gain: (1) homes per web (live range) rather than
per register, so double temporaries get XMMs; (2) a double elements kind
(numbers stored as doubles in arrays whose elements are numbers), so a
load is `movq` + `sub` with no branch; (3) bounds checks hoisted for
induction variables with known ranges; (4) loop-invariant conversions
hoisted; (5) overflow checks dropped on increments bounded by a checked
length.

## Every hot stencil, ideal code pass (2026-10-26, at 496edba)

The 155 families with no target that run at least 0.05% of the compiled
code (mean of `CLOISON_COUNT` over Octane's 15 benchmarks), judged on both
architectures. Together they account for most of the executed compiled code
the earlier passes hadn't judged. The ideal is what a person would write under the same
contract: the same value encoding, registers, frame, guards, exits and
runtime calls. A different design that would do better is noted
separately. Each judgement is a `// target` comment next to its stencil,
with the ideal sequence (abbreviated when long). There are now 284 targets
on x86-64 and 303 on arm64. `scripts/stencil-targets.py --check` passes:

| | ok | known gap | `fallthrough` |
|---|---:|---:|---:|
| x86-64 | 153 | 114 | 19 |
| arm64 | 188 | 101 | 14 |

`fallthrough <k>` marks a target for a path that LLVM didn't lay out as the
fall-through. It applies to stores whose number case or runtime call comes
first, and to arm64 `get_keyed`. The count follows the other path, of `<k>`
instructions, and the note names the path the target is for. The
families left without a target (217 on x86-64) each run less than 0.05%.

The weights below are share × gap in instructions. They rank the work and
don't predict a gain: `CLOISON_COUNT` weighs an execution by the stencil's
whole size, not by the path it took, and a layout gap costs taken branches
more than it costs instructions.

**Gaps, largest first:**

1. **The write barrier**: about 80 on x86-64, a fifth of that on arm64.
   The ideal is 15–16 instructions after the store, but about 21 are
   emitted on x86-64 and 17 on arm64, with the card mark in the
   fall-through.
   - The not-a-cell test is 9 instructions (setcc/or merged) where 4 do,
     given that every non-cell, non-number value is below 0x20; a `const`
     assert should guard that.
   - The young-host test has one instruction too many.
   - On arm64, a `sub` comes before the shift: `(p-8)>>4 == p>>4`.
   - The barrier's live values force a `push rbp`, red-zone spills, and
     r9/r15/r12 taken in the `_m`, oop, keyed and named stores.

   Writing it once as `asm goto`, with fixed scratch registers and the card
   mark cold, covers set_own_nc, sta_upvalue, set_oop_nc, set_tr_nc, set_tr,
   sta_global, set_tro_nc, set_own and sta_global_oop. It is about a day of
   work, verified under `CLOISON_GC_VERIFY`. Separately, sta_upvalue reads
   `u.at` twice; reading it once is a trivial fix.
2. **Generic `==` (`teq_j?_g/m`, `test_eq_m/g`)**: 6–9 against 37–40
   counted, about 39 on x86-64 and 33 on arm64.
   - LLVM put the both-int32 path out of line, and on the object path
     `loose_eq_fast` is branch-free where plain branches do better (40→27,
     arm64 37→28).
   - First measure which case TypeScript's `teq_jf_g2` takes. Then either
     lay out the int path first, or keep the object tests first with the
     equal-bits double case cold.
   - `tseq_jt/jf_m` (25 against 17–18, arm64 17 against 12–13) and
     `test_seq_m` (31 against 14) have the same problem. The fix: branch
     out of line on equal bits, and once the tag test has passed, test bit
     1 (`test r10b, 2` / `tbz #1`) instead of a mask.
3. **`create_array_n`**: 85 against 42 on x86-64 (about 20), 51 against 43
   on arm64. The causes: the bump check, r8/r9 spilled, `imm` loaded
   twice, and an unrolled fill loop.
   - **`create_object_<n>`** (n = 2: 21 against 17; about 7 per
     architecture) and `create_array`: `bump` should test `p + size > end`
     (`lea; cmp; ja`, not `mov; sub; cmp; jb; lea`), and zero with
     16-byte stores.
   - The earlier target of 13 for n = 2 predates the metadata byte.
4. **The cell test on x86-64**: about 14, one instruction across 12
   families (get_own, get_oop, get_proto, get_length, get_keyed*,
   guard_cell, check_shape, check_acc_shape). `tag | 2` is built with
   `mov rax, r14; or rax, 2`. Writing `tag + 2` gives `mov; add`, so the
   fix is a one-line asm `lea rax, [r14 + 2]` in `is_cell`.
5. **`get_named_rt` and `set_named_rt`**:
   - **get_named_rt**: LLVM falls through into the runtime call. The fix
     is to split it into a `__slow` piece as `get_keyed_rt_` does, and drop
     the `push rbp`. That is about 13 per architecture as counted, and 11
     (x86-64) and 3 (arm64) instructions actually executed.
   - **set_named_rt**: on x86-64, `mega_store` runs 82 instructions where 42
     do. The fix is the entry pointer computed once with early returns,
     and no barrier for `_i` (about 16).
6. **Keyed stores**:
   - **The type dispatch**: it builds a setcc boolean (`fits_code`). Making
     ANY the fall-through, with early exits, is worth about 15 on x86-64
     across set_keyed_nc, set_keyed_rt and set_keyed.
   - **`set_keyed_ap` / `_ap_i`**: they test again after the store. Bumping
     `elems_len` first frees r15/r9/r8 (about 9.5).
   - **`array_push_nc`**: 43 against 36 on x86-64. It pays the barrier's
     cell test, reloads `elems`, and does a `push rbp`.
7. **Boolean branches** (`jtofalse`, `jtotrue`, `jfalse`, `jtrue`): about 9
   on x86-64, plus 5 on arm64. GVN replaces `acc` by the constant just
   compared, giving a `mov r12d, 7` after `cmp r12, 7`. That `mov` blocks a
   direct `je L`: `jfalse` is 4 instructions against 2. `jtofalse` also
   merges its two boolean tests into `and -2; cmp 6`. A build.rs peephole
   dropping the repeated `mov` fixes all four.
8. **Calls**:
   - **`call_direct_code`** (50 against 32): LLVM made the runtime call the
     fall-through, which costs 2 taken branches per direct call. A cold
     hint on the runtime arm fixes it. `call_direct` and `construct_direct`
     probably share it.
   - **The epoch compare** in `call`, `call_done` and `call_direct_code`
     should be `cmp dword [m], imm32` (about 2.3).
   - **`test_instanceof_m`** (28 against 25): it walks a pointer through the
     cache, where fixed offsets, `imm_zext()` and `ldp` on arm64 do better.
9. **Ints in place**: `add_ii` (4 against 2), `addi_g` (5 against 3) and
   `inc_i` (4 against 2), about 6 together.
   - They should add in place with an undo exit stub, as `add_smi_i`
     does.
   - `add_smi_wrap_i` should be `add r12d, k`.
   - `add_smi_iif` needs a sign-extended imm32 (R_X86_64_32S, which
     `HoleImm` doesn't take yet).
   - `test_eq_ii` (5 against 4): writing `boolean()` as `FALSE + c` should
     also close the known `test_lt_ii` gap.
10. **Smaller, under 2 each**:
    - **x86-64**:
      - `tset_*` re-extends a key that's already zero-extended: 5
        against 4, where 2026-10-09 measured 4. The fix is `i as usize` in
        `typed_elem_x`.
      - `_chk`'s kind immediate is held in a register.
      - `spill_u` should be `lea rax, [rdi + r14]`.
      - `mul_m`'s -0 test should go out of line.
      - `bit_xor_m`, `bit_and_m` and `bit_or_smi` keep or rebuild a box
        they already had.
      - `_f` ToInt32 should use `cvttsd2si; cmp r, 1; jo`.
    - **arm64**:
      - a widened `tbz` (`tst; b.cond`) where the exit is in range;
      - `star_g{}_i` should be `orr x24, x22, x20` alone;
      - `unbox_num_acc`'s `fcsel`;
      - `tget_f32`'s NaN select;
      - `lda_smi_i` through a scratch register;
      - `get_own_nc_g{}_to_g{}` into the same slot.
    - **Both**:
      - the `_k16` immediate forms;
      - `acc | tag` boxing in the `_i`/`_ia` stores and pushes;
      - `set_frame_func_r`'s offset as a displacement.

**Earlier targets judged again:**
- **arm64 `set_own_g{}` and `set_tr_g{}`**: 25 and 29 with the barrier,
  replacing 10 and 14, which predate it.
- **arm64 `array_pop_nc_g{}`**: 12, replacing 10, which predates the
  flags check (`0xf4ff` isn't a logical immediate).
- **arm64 `tset_b32_i_g{}: 5`**: now covers b8 and b16.

**Beyond the stencils:**
- **A `Bool` type in the JIT's `Ty`**: most `jtofalse` would become
  `jfalse`, and a jump on a known object would be constant.
- **One `array_push` variant per element kind** when a shape's
  `elems_code` cannot change: it drops the 3–8 instruction dispatch.
- **One bump for both cells of an array literal.**
- **A `tbz` kept when the exit is within 32 KiB**: it would close every
  widened branch on arm64.
- **An encoder for arm64 imm12 and logical immediates**: it would close
  the `_k16` gaps, including the older `jlt_smi_i_k16` and `andi_u{}_k16`.

**Possible hazards, none reproduced:**
- **`spill_u{}_v`**: it takes any value with a zero high half for an
  int32, so undefined, null and booleans would be written back as 10, 2, 7
  or 6. That happens only if the interpreter reads such a value after an
  exit: `src/jit.rs` uses `star_u<k>_v` only for a value compiled code
  doesn't read. A probe that stores undefined into an int home and reads it
  after an exit printed undefined.
- **The stencils' `is_cell`**: it accepts 0 (the hole) where
  `Value::is_cell` does not. A hole reaching a generic `==` would read
  address 8.
