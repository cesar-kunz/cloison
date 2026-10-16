# arm64 targets: the comparison round

Status: 2026-10-10, on Apple silicon (the first arm64 hardware the project
has run on). The stencils added since group 1 (polymorphism, calls,
constructors, arrays, constants, upvalues, typed arrays, frames) held
against handwritten arm64 code, as `notes/opcode-targets.md` did for the
arithmetic, then the systematic gaps closed and the round measured.

Reproducing:

- `scripts/stencil-listing.py [name-prefix …]` lists every stencil as the
  JIT emits it (from the generated table, after `build.rs`'s rewrites)
  with its instruction count, holes and template fields. It needs
  capstone's `cstool`.
- `CLOISON_JIT_LOG=1 target/release/cloison file.js` prints each compilation's
  hot code size, so a benchmark's compiled code is `bytes / 4`
  instructions.
- `node bench/run.mjs --engine target/release/cloison --time --reps 11` times
  every microbenchmark.

Registers: `acc` x20, `regs` x21, `tag` x22 (`NUMBER_TAG`), `ctx` x23,
slots x24–x28 and x0–x6 (x24, `g0`, in the examples), a keyed store's key
x15, scratch x8–x17. Counts exclude the trailing `b cloison_continue`, which
the JIT trims when the next piece follows.

## What was systematically off, and is now fixed

Three mechanisms cost one instruction each at nearly every use. None was
specific to one stencil, so none showed in a per-opcode comparison of the
arithmetic, and all three compound in the object stencils: a property
load paid for all of them.

1. **64-bit immediates took four instructions.** A `movz` and three
   `movk` for every shape, slot address, call site and constant. Every
   address is 47 bits on Linux and macOS, so the fourth part was always
   zero. `imm64_of!` is now `movz` + two `movk` (48 bits), and the JIT
   asserts the value fits when patching. Holes that can hold any 64 bits,
   a folded constant, an exit's constant, the word `guard_word` and
   `ctor_check` expect, and `mod_smi`'s multiplier, keep the fourth part
   (`imm64_full_of!`). −1 at 300 stencils.
2. **A property slot was `movz` + a register-indexed load.** The offset
   hole could only be an absolute `movz`, and LLVM indexed with it. The
   access is now the asm itself, `ldr x20, [x24, #off]`, with the offset
   in the instruction's `imm12` through a `:lo12:` relocation
   (`LDST64_ABS_LO12_NC`, whose field the JIT now patches with the whole
   offset: below 32 KiB, which it checks, falling back as it did past 64
   KiB) or Mach-O's `@PAGEOFF`. −1 per slot access (`ldst_imm!`).
3. **A memory-home register likewise.** `ldar_m`, `star_m`, `enter_*`,
   `spill_*`, every `_m` stencil: `ldr x20, [x21, #off]` now, and an
   int32 home enters its slot with one `ldr w` (`load_m_unboxed`).

The x86-64 stencils are byte for byte what they were (checked against the
previous table): x86-64 folds all three into addressing modes and
`movabs`, which is why they were never visible there.

Across the arm64 table: 9,822 → 9,442 instructions, 308 stencils
shorter, none longer.

## Stencils against the targets

Before → now → target. The target is what the stencil mechanism can
express today; "further" is what a different mechanism would give.

### Guards and property access

`check_shape_g` (the receiver is a cell of shape `imm64`): 10 → 9 → 9.

```text
orr   x8, x22, #2            ; NOT_CELL_MASK: the tag plus bit 1 (no
tst   x24, x8                ;   logical immediate encodes it)
b.ne  exit
ldr   x8, [x24]
movz  x9, #s0 ; movk x9, #s1, lsl #16 ; movk x9, #s2, lsl #32
cmp   x8, x9
b.ne  exit
```

Further: the mask in a pinned register (2 instructions for the cell test;
it would need a seventh state register), the shape as a literal after the
piece (`ldr x9, lit`: 7), or 32-bit shape IDs (`ldr w8; movz; movk; cmp
w8, w9`: 8).

| stencil | before | now | target | notes |
|---|---:|---:|---:|---|
| `get_own_g` | 12 | 10 | 10 | guard + `ldr x20, [x24, #off]` |
| `get_own_nc_g` | 2 | 1 | 1 | |
| `get_oop_nc_g` | 3 | 2 | 2 | `ldr x8, [x24, #16]; ldr x20, [x8, #off]` |
| `get_proto_g` | 15 | 13 | 13 | guard, then the slot's address (3) and `ldr`; a literal: 11 |
| `get_proto_nc_g` | 5 | 4 | 4 | a literal: 2 |
| `set_own_g` | 12 | 10 | 10 | |
| `set_tr_g` | 17 | 14 | 14 | the new shape (3), `str` it, `str` the value |
| `shape_guard_g` | 7 | 6 | 6 | |
| `family_guard_g` | 8 | 7 | 7 | `ldr x8, [x24]; ldr x8, [x8, #8]`, family (3), `cmp`, `b.ne` |
| `guard_cell_g` | 3 | 3 | 3 | 2 with the mask pinned |
| `guard_word` | 11 | 10 | 10 | address (3), `ldr`, word (4: any value), `cmp`, `b.ne`; 9 when the JIT knows the word is a pointer |
| `ctor_check` | 12 | 11 | 11 | `guard_word` + `mov w20, #undefined` |
| `check_callee_g`, `callee_jne_g` | 6 | 5 | 5 | callee (3), `cmp`, branch |
| `lda_constant` | 4 | 4 | 4 | a `Value`: 64 bits; a literal: 1 |
| `lda_global` | 13 | 10 | 10 | global object (3), `ldr` shape, shape (3), `cmp`, `b.ne`, `ldr x20, [x8, #off]` |
| `lda_upvalue` | 5 | 4 | 4 | `ldur` func, `ldr [x8, #off]`, `ldr`, `ldr` |
| `lda_upvalue_c` | 6 | 5 | 5 | cell (3), `ldr`, `ldr` |

### Calls, constructors, frames

| stencil | before | now | target | notes |
|---|---:|---:|---:|---|
| `call_done` | 10 | 9 | 8 | `cbz`, epoch address (3), `ldr w`, epoch (2), `cmp`, `b.ne`; a `_k16` epoch: 8 |
| `create_object_N` | 18 | 16 | 16 | heap (3), `ldp`, `sub`, `cmp`, `b.lo`, `movi`, `add`, `str`, `mov w9, #0x100`, shape (3), `stp`, `stp q8, q8` |
| `create_array` | 10 | 9 | 9 | a runtime call |
| `materialize` | 9 | 8 | 8 | a runtime call |
| `set_frame_func` | 7 | 6 | 6 | function (3), offset, `str` |
| `enter_g`, `enter_u` | 2 | 1 | 1 | `ldr x24, [x21, #off]` / `ldr w24, …` |
| `enter_f_i` | 3 | 2 | 2 | `ldr w8`, `scvtf` |
| `spill_g` | 2 | 1 | 1 | |
| `spill_u` | 3 | 2 | 2 | `orr x8, x24, x22; str` |
| `ldar_m`, `star_m` | 2 | 1 | 1 | |
| `star_m_i` | 3 | 2 | 2 | `orr x20, x22, x20; str` |
| `exit` | 5 | 5 | 4 | the bytecode offset in one `movz` when it fits (`_k16`); cold |
| `return` | 2 | 2 | 2 | |

The direct call itself (`arch::direct_call`) and the runtime-call and
enter wrappers are hand-assembled in `src/jit.rs`, so they are their own
targets; they were checked under qemu when written.

### Arrays and keyed access

| stencil | before | now | target | notes |
|---|---:|---:|---:|---|
| `get_length_nc_g` | 1 | 1 | 1 | |
| `get_keyed_i_nc_g` | 7 | 7 | 7 | the load lands in x8 and moves to x20 after the hole check: an exit needs `acc` (the key) intact |
| `array_push_i_nc_g` | 13 | 13 | 12 | `mov w10, w20; orr x10, x22, x10` boxes the int32: with `acc`'s invariant (high bits 0 or the tag) `orr x10, x22, x20` alone does |
| `array_push_nc_g` | 26 | 26 | 26 | four-way elements' type dispatch; the ideal is the same shape |
| `array_pop_nc_g` | 13 | 13 | 10 | LLVM indexes `elems + len*8` then `ldur`/`stur` at −8; `sub` first and `ldr`/`str [x9, w8, uxtw #3]` saves the `add` and the two unscaled forms |
| `set_keyed_nc_i_g` | 10 | 10 | 9 | the same redundant `mov w8, w20` |
| `tget_i32_g`, `tset_b32_i_g` | 5 | 5 | 5 | |
| `key_u` | 1 | 1 | 1 | |

### Arithmetic with immediates (from group 1)

| stencil | now | target | notes |
|---|---:|---:|---|
| `jlt_smi_i_k16` | 3 | 2 | `cmp w20, #k; b.lt`: needs the immediate in `imm12` |
| `addi_u_k16` | 3 | 2 | `adds w24, w24, #k; b.vs` |
| `incjlt_u_k16` | 5 | 4 | `adds; b.vs; cmp w24, #k; b.lt` |
| `andi_u_k16` | 2 | 1 | `and w24, w24, #k` for a logical immediate |

These need `imm12` fields the assembler won't relocate: `#:lo12:` and
`@PAGEOFF` are accepted on `add` only, not on `adds`, `cmp` or `and`
(checked). The mechanism is the one the macOS holes use, a magic
immediate that `build.rs` turns into a relocation, and `_k12` variants
the JIT picks when the constant fits. Not done in this round.

## Benchmarks

Every microbenchmark on Apple silicon, before and after the three fixes: the
hot code the JIT compiled for it in all (`CLOISON_JIT_LOG`, bytes / 4), and
its time as a whole process (`bench/run.mjs --time`, the median of 11
runs, 0.3 ms of it startup). Times on this machine vary by about ±5%
from run to run; the two files that got slower have fewer instructions.

| benchmark | hot instructions before | now | Δ | time before (ms) | now | Δ |
|---|---:|---:|---:|---:|---:|---:|
| 01-int-loop | 17 | 15 | -11.8% | 3.79 | 3.32 | -12.4% |
| 02-int-compare-ternary | 122 | 118 | -3.3% | 4.0 | 4.11 | +2.8% |
| 03-double-scale | 62 | 58 | -6.5% | 3.59 | 3.83 | +6.7% |
| 04-int-to-double-field | 170 | 155 | -8.8% | 3.87 | 3.46 | -10.6% |
| 05-logical-or-default | 66 | 62 | -6.1% | 3.94 | 3.56 | -9.6% |
| 06-switch-int | 97 | 93 | -4.1% | 4.61 | 4.02 | -12.8% |
| 07-string-concat-number | 42 | 40 | -4.8% | 6.43 | 6.24 | -3.0% |
| 08-do-while | 54 | 51 | -5.6% | 4.13 | 3.71 | -10.2% |
| 09-loose-equality | 127 | 123 | -3.1% | 5.02 | 4.54 | -9.6% |
| 10-call-global-function | 45 | 43 | -4.4% | 4.5 | 4.06 | -9.8% |
| 11-call-static-method | 59 | 54 | -8.5% | 4.16 | 3.68 | -11.5% |
| 12-call-this-static | 81 | 72 | -11.1% | 4.74 | 4.01 | -15.4% |
| 13-missing-arguments | 548 | 498 | -9.1% | 6.56 | 6.17 | -5.9% |
| 14-parent-constructor-call | 116 | 111 | -4.3% | 4.33 | 3.91 | -9.7% |
| 15-super-method-call | 160 | 142 | -11.2% | 4.69 | 3.77 | -19.6% |
| 16-inner-function | 42 | 42 | +0.0% | 16.54 | 15.74 | -4.8% |
| 17-constructor-side-effects | 332 | 299 | -9.9% | 6.47 | 6.11 | -5.6% |
| 18-constructor-calls-override | 1748 | 1600 | -8.5% | 7.15 | 6.78 | -5.2% |
| 19-cold-path-throw | 114 | 104 | -8.8% | 5.88 | 4.77 | -18.9% |
| 20-constructor-fields | 138 | 134 | -2.9% | 7.05 | 6.58 | -6.7% |
| 21-monomorphic-get | 66 | 59 | -10.6% | 4.96 | 4.23 | -14.7% |
| 22-monomorphic-set | 163 | 153 | -6.1% | 5.05 | 4.4 | -12.9% |
| 23-prototype-method | 31 | 27 | -12.9% | 6.08 | 3.58 | -41.1% |
| 24-inherited-method | 28 | 24 | -14.3% | 3.99 | 3.41 | -14.5% |
| 25-polymorphic-get | 89 | 77 | -13.5% | 5.31 | 4.59 | -13.6% |
| 26-polymorphic-call | 90 | 79 | -12.2% | 5.51 | 4.78 | -13.2% |
| 27-shared-method-two-layouts | 126 | 108 | -14.3% | 6.45 | 5.91 | -8.4% |
| 28-define-on-object-prototype | 44 | 40 | -9.1% | 28.36 | 25.3 | -10.8% |
| 29-property-miss | 35 | 30 | -14.3% | 3.48 | 3.12 | -10.3% |
| 30-new-object-constants | 52 | 50 | -3.8% | 4.71 | 4.17 | -11.5% |
| 31-constant-props-in-methods | 79 | 68 | -13.9% | 4.16 | 3.68 | -11.5% |
| 32-identity-singletons | 67 | 62 | -7.5% | 4.37 | 3.68 | -15.8% |
| 33-null-fields | 97 | 87 | -10.3% | 4.28 | 3.64 | -15.0% |
| 34-chained-loads | 107 | 80 | -25.2% | 4.98 | 4.24 | -14.9% |
| 35-chained-assignment | 51 | 40 | -21.6% | 4.85 | 3.94 | -18.8% |
| 36-boolean-and-chain | 80 | 67 | -16.2% | 5.29 | 4.71 | -11.0% |
| 37-prefix-increment-field | 30 | 24 | -20.0% | 5.63 | 3.32 | -41.0% |
| 38-mixed-depth-polymorphic-call | 275 | 235 | -14.5% | 10.26 | 9.75 | -5.0% |
| 39-empty-polymorphic-methods | 202 | 181 | -10.4% | 7.89 | 7.16 | -9.3% |
| 40-array-push-length | 77 | 71 | -7.8% | 4.82 | 4.44 | -7.9% |
| 41-array-pop-worklist | 213 | 204 | -4.2% | 4.79 | 4.3 | -10.2% |
| 42-array-index-load | 38 | 34 | -10.5% | 3.74 | 3.36 | -10.2% |
| 43-array-compact-remove | 442 | 424 | -4.1% | 4.72 | 4.23 | -10.4% |
| 44-array-scan-filter | 129 | 120 | -7.0% | 4.83 | 4.48 | -7.2% |
| 45-collection-wrapper | 213 | 194 | -8.9% | 4.11 | 3.65 | -11.2% |
| 46-polymorphic-elements-scan | 727 | 650 | -10.6% | 15.29 | 13.72 | -10.3% |
| 50-global-object-in-hot-method | 103 | 89 | -13.6% | 6.24 | 5.56 | -10.9% |
| 51-global-reassigned | 50 | 41 | -18.0% | 6.14 | 5.7 | -7.2% |
| 52-global-store-number | 129 | 115 | -10.9% | 5.64 | 5.09 | -9.8% |
| 53-top-level-loop | 94 | 76 | -19.1% | 4.08 | 3.51 | -14.0% |
| 54-global-reentrant-calls | 2394 | 2069 | -13.6% | 11.4 | 10.24 | -10.2% |
| 60-local-worklist-wrapper | 514 | 475 | -7.6% | 4.28 | 3.75 | -12.4% |
| 61-worklist-passed-to-helper | 509 | 464 | -8.8% | 5.27 | 4.78 | -9.3% |
| 62-returned-collection | 323 | 299 | -7.4% | 4.56 | 4.05 | -11.2% |
| 63-consume-callers-collection | 1070 | 989 | -7.6% | 5.83 | 5.37 | -7.9% |
| 64-owned-child | 239 | 213 | -10.9% | 5.22 | 4.63 | -11.3% |
| 65-dead-field | 62 | 59 | -4.8% | 8.36 | 8.08 | -3.3% |
| 66-graph-by-constructors | 956 | 860 | -10.0% | 5.61 | 5.07 | -9.6% |
| 67-arg-to-external-method | 211 | 197 | -6.6% | 4.82 | 4.01 | -16.8% |
| 68-externalize-graph-once | 1154 | 1073 | -7.0% | 6.65 | 6.24 | -6.2% |
| 69-region-per-call | 242 | 224 | -7.4% | 8.18 | 7.61 | -7.0% |
| 70-alloc-in-loop-prev-live | 104 | 92 | -11.5% | 5.9 | 5.54 | -6.1% |
| 71-short-lived-per-call | 1350 | 1198 | -11.3% | 7.07 | 6.63 | -6.2% |
| 80-plan-execute-copy | 438 | 348 | -20.5% | 4.74 | 4.15 | -12.4% |
| 81-plan-execute-scale | 548 | 462 | -15.7% | 4.59 | 4.17 | -9.2% |
| 82-propagate-walk | 1420 | 1272 | -10.4% | 6.55 | 5.86 | -10.5% |
| 83-collect-and-readd | 534 | 486 | -9.0% | 5.31 | 4.83 | -9.0% |
| 84-build-and-tear-down | 1530 | 1400 | -8.5% | 8.79 | 8.01 | -8.9% |
| 85-plan-extraction | 1256 | 1118 | -11.0% | 9.74 | 8.81 | -9.5% |
| 86-alternating-phases | 3202 | 2859 | -10.7% | 8.77 | 8.65 | -1.4% |
| 87-constraint-solver | 72905 | 64278 | -11.8% | 44.71 | 40.97 | -8.4% |
| 90-recursive-fib | 347 | 334 | -3.7% | 4.55 | 4.1 | -9.9% |
| 91-mutual-recursion | 1096 | 1058 | -3.5% | 5.16 | 4.18 | -19.0% |
| 92-tail-recursion | 48 | 45 | -6.2% | 4.1 | 3.39 | -17.3% |
| 93-constructor-loop | 122 | 116 | -4.9% | 3.3 | 3.27 | -0.9% |
| 94-constructor-and-native | 129 | 121 | -6.2% | 4.09 | 3.88 | -5.1% |

Geometric mean of the ratios: hot instructions: 0.902x; Geometric mean of the ratios: time: 0.890x; total hot 100770 -> 89432.

Hot code: −9.8% of instructions overall, −11% of time. The object
benchmarks gain most (23, 37: −41%, whose loops are a property access and
little else; 34, 35, 80: −20% of their code); the arithmetic loops least,
since their `_k16` immediates and slot registers were already right.


## Gaps left, ranked by what they cost

1. **`imm12` immediates** (above): a loop tail of a counted loop with a
   small bound is 5 instructions where 4 would do, and every `jlt_smi` /
   `addi` with a constant below 4096 one more than needed.
2. **The boxing of an int32 `acc`** in `array_push_i` and `set_keyed_i`:
   `mov w, w; orr` where `orr` alone does. One instruction per push and
   keyed store. A source change (`rebox` instead of `from_i32`) that
   would also change the x86-64 bytes, so it waits for an x86-64 round.
3. **`array_pop`'s addressing**: 3 instructions of LLVM's choosing.
4. **Literal pools for shapes and addresses**: a `ldr x9, lit` where
   three `movz`/`movk` go, with the literal placed after the piece (the
   JIT lays pieces out, so it can). Two instructions per guard, but a
   load on the guard's critical path, so it needs measuring.
5. **The cell test** needs `NOT_CELL_MASK` in a register: 1 instruction
   per guard, at the cost of a seventh state register in every stencil's
   signature (or x16/x17, which calls clobber).
