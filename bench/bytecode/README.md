# Expected bytecode

Hand-written bytecode for some of the [microbenchmarks](../micro/), in
toi's text assembly format (see [src/asm.rs](../../src/asm.rs)). Each
listing is what a straightforward JS-to-bytecode compiler should emit for
the benchmark of the same name: no escape analysis, no inlining, no type
specialization. The listings serve two purposes:

- They run the object model and the interpreter before there is a front
  end: `target/release/toi bench/bytecode/21-monomorphic-get.asm`.
- They are the reference for the bytecode compiler. Its output for
  `micro/N.js` should match `N.asm` up to register numbering, or differ
  for a reason worth writing down.

`tests/bench_bytecode.rs` runs every listing and checks the result against
its benchmark's `// expect:` line. `run.mjs` takes listings too:

```sh
cargo build --release
node bench/run.mjs --engine target/release/toi --time bench/bytecode/*.asm
```

| listing | benchmark | exercises |
|---|---|---|
| [01-int-loop.asm](01-int-loop.asm) | [01](../micro/01-int-loop.js) | int32 arithmetic in the `I32` accumulator state, `BitOrSmi`, a rotated loop |
| [10-call-global-function.asm](10-call-global-function.asm) | [10](../micro/10-call-global-function.js) | `LdaGlobal` hits, `CallUndefined`, frames, `ModSmi`, `ToBoolean` jumps |
| [21-monomorphic-get.asm](21-monomorphic-get.asm) | [21](../micro/21-monomorphic-get.js) | `Construct`, store transitions in a constructor, a native call (`push`), `GetKeyed`, own-property `GetNamed` hits |
| [23-prototype-method.asm](23-prototype-method.asm) | [23](../micro/23-prototype-method.js) | prototype-chain `GetNamed` hits (the method), `Call` with a receiver, a field updated in place |
| [34-chained-loads.asm](34-chained-loads.asm) | [34](../micro/34-chained-loads.js) | chains of `GetNamed`, `TestLessThan` + `JumpIfFalse`, `SetNamed` replacing object-valued fields |
| [90-recursive-fib.asm](90-recursive-fib.asm) | [90](../micro/90-recursive-fib.js) | a function calling itself twice (direct self-calls, inlined once) |
| [91-mutual-recursion.asm](91-mutual-recursion.asm) | [91](../micro/91-mutual-recursion.js) | two functions calling each other (compiled together) |
| [92-tail-recursion.asm](92-tail-recursion.asm) | [92](../micro/92-tail-recursion.js) | a self tail call (a loop), called from a loop |
| [93-constructor-loop.asm](93-constructor-loop.asm) | [93](../micro/93-constructor-loop.js) | `Construct` in a loop (direct, `this` allocated in the stencil), store transitions |
| [94-constructor-and-native.asm](94-constructor-and-native.asm) | [94](../micro/94-constructor-and-native.js) | `Construct` and a native `Call` (`push`) in one loop |

## The format

```text
; comment
script                          ; the top-level code, run with this = the global object
        CreateClosure main      ; a function defined below
        StaGlobal main          ; inline-cache operands name the property
        …
end

function main()                 ; parameter names only document (r1, r2, …)
        LdaSmi 0
loop:   …                       ; a label, optionally followed by an instruction
        JumpIfLessSmi 10, loop  ; a backward jump takes the loop form (…Loop)
        Return
end
```

A function's register count is the highest register it names (call
argument windows included), plus one. Every inline-cache operand gets its
own slot, and every `CreateClosure` its function.

## Conventions the listings assume of the compiler

- **Registers**: `r0` is `this`, then the parameters, then `var`s in order of
  declaration, then temporaries, allocated as a stack per expression.
- **Operands**: binary operators put the left operand in a register and the
  right one in the accumulator (`Add r` computes `r + acc`), as Ignition
  does, so `n += e` needs no temporary: `e` into acc, `Add r_n`, `Star r_n`.
  The `…Smi` forms take a literal right operand: `acc + imm`.
- **Hoisting**: the script starts by creating every top-level function and
  storing it as a global.
- **Loops are rotated**: `init; Jump test; body: …; update; test: …;
  JumpIf… body`. A `x < literal` condition becomes a fused
  `JumpIfLessSmi`; the backward jump is the loop's back edge.
- **Conditions**: a comparison yields a boolean, so it's tested with
  `JumpIfTrue`/`JumpIfFalse`; any other value is tested with
  `JumpIfToBooleanTrue`/`False`. `a || b` in a condition jumps on `a`.
- **Calls**: the callee goes in a temporary and the arguments in
  consecutive temporaries (for `Call`, the receiver first). One argument
  that's already in a register is passed in place: `new V(i)` is
  `Construct r_V, r_i, 1`.
- **Peepholes within a basic block only**: a value already in the
  accumulator is not reloaded (`LdaSmi 0; Star r1; Star r2`), but nothing
  is assumed across a label.
- **Statements**: `x++` as a statement is `Ldar; Inc; Star`; a function
  without `return` ends in `LdaUndefined; Return`; the script returns the
  value of its last expression statement.
