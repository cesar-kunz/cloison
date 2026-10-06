//! Every program runs on four engines: the interpreter, the JIT with
//! generic stencils (milestone 1), with type-specialized stencils
//! (milestone 2), and with VM registers in machine registers (milestone 3).
//! They must agree on the result and on the final register file.

use toi::bytecode::Assembler;
use toi::jit::JitFunction;
use toi::value::Value;

/// The same JS value. Numbers compare by value, so int32 `3` matches
/// double `3.0` (the specialized JIT may keep a number as a double where the
/// interpreter has an int32), but `0` and `-0` differ, and any two NaNs
/// match. Everything else compares bitwise.
fn same(a: Value, b: Value) -> bool {
    match (a.as_number(), b.as_number()) {
        (Some(x), Some(y)) => {
            (x == y && x.is_sign_negative() == y.is_sign_negative()) || (x.is_nan() && y.is_nan())
        }
        _ => a == b,
    }
}

fn exec(a: Assembler, regs: &mut [Value]) -> Value {
    let code = a.finish();
    let initial = regs.to_vec();
    let interp = unsafe { toi::interp::run(&code, regs) };
    for (name, mut f) in [
        ("jit1", JitFunction::new(&code).unspecialized()),
        ("jit2", JitFunction::new(&code).without_register_homes()),
        ("jit", JitFunction::new(&code)),
    ] {
        let mut jit_regs = initial.clone();
        let jit = unsafe { f.run(&mut jit_regs) };
        assert!(
            same(interp, jit),
            "interp returned {interp:?}, {name} {jit:?}"
        );
        for (r, (i, j)) in regs.iter().zip(&jit_regs).enumerate() {
            assert!(same(*i, *j), "r{r}: interp {i:?}, {name} {j:?}");
        }
    }
    interp
}

fn sum_loop(n: i32) -> Assembler {
    let (sum, i, limit) = (0, 1, 2);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(sum).star(i);
    a.lda_smi(n).star(limit);
    a.bind(top);
    a.ldar(sum).add(i).star(sum);
    a.ldar(i).inc().nop().star(i);
    a.jump_if_less(limit, top);
    a.ldar(sum).ret();
    a
}

#[test]
fn straight_line() {
    let mut a = Assembler::new();
    a.lda_smi(40).star(0).lda_smi(2).add(0).ret();
    assert_eq!(exec(a, &mut [Value::UNDEFINED]), Value::from_i32(42));
}

#[test]
fn negative_immediate() {
    let mut a = Assembler::new();
    a.lda_smi(-7).ret();
    assert_eq!(exec(a, &mut []), Value::from_i32(-7));
}

#[test]
fn forward_and_backward_jumps() {
    // Jump over a block, then jump back into it, then out.
    let mut a = Assembler::new();
    let (skip, back, out) = (a.label(), a.label(), a.label());
    a.jump(skip);
    a.bind(back);
    a.lda_smi(99).jump(out);
    a.bind(skip);
    a.lda_smi(1).jump(back);
    a.bind(out);
    a.ret();
    assert_eq!(exec(a, &mut []), Value::from_i32(99));
}

#[test]
fn loop_does_not_grow_the_stack() {
    // 10M iterations × 8 tail calls each. Without guaranteed TCO this would
    // overflow the stack, in debug builds too. The sum leaves int32 range
    // early, so this also covers int → double transitions in a loop.
    let n = 10_000_000i64;
    let r = exec(sum_loop(n as i32), &mut [Value::UNDEFINED; 3]);
    assert!(r.is_double());
    assert_eq!(r.as_number(), Some((n * (n - 1) / 2) as f64));
}

#[test]
fn add_overflow_becomes_double() {
    let mut a = Assembler::new();
    a.lda_smi(1).star(0).lda_smi(i32::MAX).add(0).ret();
    let r = exec(a, &mut [Value::UNDEFINED]);
    assert!(r.is_double());
    assert_eq!(r.as_number(), Some(2147483648.0));
}

#[test]
fn inc_overflow_becomes_double() {
    let mut a = Assembler::new();
    a.lda_smi(i32::MAX).inc().ret();
    assert_eq!(exec(a, &mut []).as_number(), Some(2147483648.0));
}

#[test]
fn double_plus_double() {
    // Like LLInt, the number slow path doesn't renormalise 3.0 to int32.
    let mut a = Assembler::new();
    a.ldar(0).add(0).ret();
    let r = exec(a, &mut [Value::from_f64(1.5)]);
    assert_eq!(r.as_number(), Some(3.0));
}

#[test]
fn double_overflow_and_nan() {
    let mut a = Assembler::new();
    a.ldar(0).add(1).ret();
    let r = exec(
        a,
        &mut [
            Value::from_f64(f64::INFINITY),
            Value::from_f64(f64::NEG_INFINITY),
        ],
    );
    assert!(r.is_double() && r.as_number().unwrap().is_nan(), "{r:?}");
}

#[test]
fn add_primitives() {
    // undefined + 1 = NaN, null + 1 = 1, true + 1 = 2.
    let cases = [
        (Value::UNDEFINED, f64::NAN),
        (Value::NULL, 1.0),
        (Value::TRUE, 2.0),
        (Value::FALSE, 1.0),
    ];
    for (v, expected) in cases {
        let mut a = Assembler::new();
        a.lda_smi(1).add(0).ret();
        let r = exec(a, &mut [v]).as_number().unwrap();
        assert!(
            r == expected || (r.is_nan() && expected.is_nan()),
            "{v:?} + 1 = {r}"
        );
    }
}

#[test]
fn inc_undefined_is_nan() {
    let mut a = Assembler::new();
    a.ldar(0).inc().ret();
    assert!(
        exec(a, &mut [Value::UNDEFINED])
            .as_number()
            .unwrap()
            .is_nan()
    );
}

#[test]
fn jump_if_less_slow_paths() {
    // Returns 1 if r0 < r1, else 0.
    let lt = |x: Value, y: Value| {
        let mut a = Assembler::new();
        let yes = a.label();
        a.ldar(0).jump_if_less(1, yes);
        a.lda_smi(0).ret();
        a.bind(yes);
        a.lda_smi(1).ret();
        exec(a, &mut [x, y]) == Value::from_i32(1)
    };
    let (one, two) = (Value::from_i32(1), Value::from_i32(2));
    assert!(lt(one, two) && !lt(two, one) && !lt(one, one));
    assert!(lt(Value::from_f64(1.5), two));
    assert!(!lt(two, Value::from_f64(1.5)));
    assert!(lt(Value::from_i32(-1), Value::from_f64(-0.5)));
    // NaN compares false both ways.
    assert!(!lt(Value::UNDEFINED, one) && !lt(one, Value::UNDEFINED));
    assert!(lt(Value::NULL, one) && lt(Value::FALSE, Value::TRUE));
}

// ---- Immediate-operand opcodes and I32 accumulator-state transitions ----
// With `acc-i32`, `lda_smi`/`add`/`inc` leave the accumulator unboxed in the
// I32 state; these cover every way out of it. They also run without the
// feature, where they exercise the plain ANY handlers.

#[test]
fn add_smi() {
    let mut a = Assembler::new();
    a.lda_smi(40).add_smi(2).ret();
    assert_eq!(exec(a, &mut []), Value::from_i32(42));
    // From ANY (after ldar), and negative immediates.
    let mut a = Assembler::new();
    a.ldar(0).add_smi(-50).ret();
    assert_eq!(exec(a, &mut [Value::from_i32(8)]), Value::from_i32(-42));
}

#[test]
fn add_smi_slow_paths() {
    let mut a = Assembler::new();
    a.lda_smi(i32::MAX).add_smi(1).ret();
    assert_eq!(exec(a, &mut []).as_number(), Some(2147483648.0));
    let mut a = Assembler::new();
    a.lda_smi(i32::MIN).add_smi(-1).ret();
    assert_eq!(exec(a, &mut []).as_number(), Some(-2147483649.0));
    let mut a = Assembler::new();
    a.ldar(0).add_smi(1).ret();
    assert_eq!(exec(a, &mut [Value::from_f64(0.5)]).as_number(), Some(1.5));
    assert_eq!(exec_add_smi_on(Value::NULL), Some(1.0));
}

fn exec_add_smi_on(v: Value) -> Option<f64> {
    let mut a = Assembler::new();
    a.ldar(0).add_smi(1).ret();
    exec(a, &mut [v]).as_number()
}

#[test]
fn jump_if_less_smi() {
    // Returns 1 if r0 < 10, else 0.
    let lt10 = |x: Value| {
        let mut a = Assembler::new();
        let yes = a.label();
        a.ldar(0).jump_if_less_smi(10, yes);
        a.lda_smi(0).ret();
        a.bind(yes);
        a.lda_smi(1).ret();
        exec(a, &mut [x]) == Value::from_i32(1)
    };
    assert!(lt10(Value::from_i32(9)) && !lt10(Value::from_i32(10)));
    assert!(lt10(Value::from_i32(i32::MIN)) && !lt10(Value::from_i32(i32::MAX)));
    assert!(lt10(Value::from_f64(9.5)) && !lt10(Value::from_f64(10.5)));
    assert!(!lt10(Value::UNDEFINED) && lt10(Value::NULL));
}

#[test]
fn i32_state_overflow_recovers_original_acc() {
    // acc is unboxed in I32 when the add overflows; the slow path must see
    // the original operands, not the wrapped sum.
    let mut a = Assembler::new();
    a.lda_smi(i32::MAX - 1)
        .star(0)
        .lda_smi(i32::MAX)
        .add(0)
        .ret();
    assert_eq!(
        exec(a, &mut [Value::UNDEFINED]).as_number(),
        Some(2.0 * i32::MAX as f64 - 1.0)
    );
    let mut a = Assembler::new();
    a.lda_smi(i32::MIN).star(0).add(0).ret();
    assert_eq!(
        exec(a, &mut [Value::UNDEFINED]).as_number(),
        Some(2.0 * i32::MIN as f64)
    );
}

#[test]
fn i32_state_add_with_non_int_rhs() {
    let mut a = Assembler::new();
    a.lda_smi(1).add(0).ret();
    assert_eq!(
        exec(a, &mut [Value::from_f64(0.25)]).as_number(),
        Some(1.25)
    );
}

#[test]
fn i32_state_jump_with_non_int_rhs() {
    let mut a = Assembler::new();
    let yes = a.label();
    a.lda_smi(1).jump_if_less(0, yes);
    a.lda_smi(0).ret();
    a.bind(yes);
    a.lda_smi(1).ret();
    assert_eq!(exec(a, &mut [Value::from_f64(1.5)]), Value::from_i32(1));
}

#[test]
fn i32_state_star_stores_boxed_values() {
    // Unboxed results must be boxed when stored, including negative ones
    // (whose unboxed form has high bits clear).
    let mut regs = [Value::UNDEFINED; 3];
    let mut a = Assembler::new();
    a.lda_smi(-5)
        .inc()
        .star(0)
        .add_smi(10)
        .star(1)
        .add(1)
        .star(2)
        .ret();
    let r = exec(a, &mut regs);
    assert_eq!(
        regs,
        [Value::from_i32(-4), Value::from_i32(6), Value::from_i32(12)]
    );
    assert_eq!(r, Value::from_i32(12));
}

#[test]
fn i32_state_survives_jumps_and_nops() {
    let mut a = Assembler::new();
    let (l1, l2) = (a.label(), a.label());
    a.lda_smi(1).jump(l1);
    a.bind(l2);
    a.inc().ret();
    a.bind(l1);
    a.nop().inc().jump(l2);
    assert_eq!(exec(a, &mut []), Value::from_i32(3));
}

// ---- JIT type specialization (milestone 2) ----

fn jit(a: &Assembler, regs: &mut [Value]) -> (Value, JitFunction) {
    let code = a.clone().finish();
    let mut f = JitFunction::new(&code);
    let v = unsafe { f.run(regs) };
    (v, f)
}

fn stencils(f: &JitFunction) -> Vec<String> {
    f.last_code()
        .unwrap()
        .stencils
        .iter()
        .flatten()
        .cloned()
        .collect()
}

/// Whether `suffix` is a location: `m`, `g<k>` or `f<k>`.
fn is_loc(suffix: &str) -> bool {
    suffix == "m"
        || suffix
            .strip_prefix(['g', 'f'])
            .is_some_and(|k| !k.is_empty() && k.bytes().all(|b| b.is_ascii_digit()))
}

/// Whether some selected stencil is `name`, or `name` at some location.
fn has(s: &[String], name: &str) -> bool {
    s.iter().any(|n| {
        n == name
            || n.strip_prefix(name)
                .and_then(|r| r.strip_prefix('_'))
                .is_some_and(is_loc)
    })
}

#[test]
fn int_loop_is_fully_specialized() {
    let a = sum_loop(1000);
    let (v, f) = jit(&a, &mut [Value::UNDEFINED; 3]);
    assert_eq!(v, Value::from_i32(499500));
    assert_eq!((f.stats.compiles, f.stats.exits), (1, 0));
    let s = stencils(&f);
    assert!(
        has(&s, "add_ii") && has(&s, "inc_i") && has(&s, "jlt_ii"),
        "{s:?}"
    );
    assert!(
        !["add", "inc", "jlt"].iter().any(|g| has(&s, g)),
        "generic stencil in {s:?}"
    );
}

#[test]
fn overflow_exits_once_then_resumes_as_double() {
    // The sum leaves int32 after ~65k iterations: one exit at `add`, one
    // recompile entering mid-loop with `s` converted to double.
    let n = 100_000i64;
    let (v, f) = jit(&sum_loop(n as i32), &mut [Value::UNDEFINED; 3]);
    assert_eq!(v.as_number(), Some((n * (n - 1) / 2) as f64));
    assert_eq!((f.stats.compiles, f.stats.exits), (2, 1));
    let s = stencils(&f);
    assert!(has(&s, "add_fi") || has(&s, "add_ff"), "{s:?}");
    assert!(has(&s, "inc_i"), "{s:?}");
}

#[test]
fn widening_join_converts_on_a_jump_edge() {
    // `s` enters the loop as int32 through a jump to the condition, and comes
    // around as a double from the body. The join at `cond` is F64, so the
    // jump edge needs an int32 → double conversion stub.
    let (s, i, d) = (0, 1, 2);
    let mut a = Assembler::new();
    let (top, cond) = (a.label(), a.label());
    a.lda_smi(0).star(s).star(i).jump(cond);
    a.bind(top);
    a.ldar(s).add(d).star(s);
    a.ldar(i).inc().star(i);
    a.bind(cond);
    a.ldar(i).jump_if_less_smi(10, top);
    a.ldar(s).ret();
    let mut regs = [Value::UNDEFINED, Value::UNDEFINED, Value::from_f64(0.5)];
    assert_eq!(exec(a.clone(), &mut regs.clone()).as_number(), Some(5.0));
    let (v, f) = jit(&a, &mut regs);
    assert_eq!(v.as_number(), Some(5.0));
    assert_eq!((f.stats.compiles, f.stats.exits), (1, 0));
    assert!(has(&stencils(&f), "add_ff"), "{:?}", stencils(&f));
}

#[test]
fn entry_types_come_from_the_live_values() {
    // The same code, entered with an int32, a double and undefined in r0.
    let mut a = Assembler::new();
    a.ldar(0).add_smi(1).ret();
    for (v, stencil, expected) in [
        (Value::from_i32(1), "add_smi_i", 2.0),
        (Value::from_f64(1.5), "add_smi_f", 2.5),
        (Value::NULL, "add_smi", 1.0),
    ] {
        let (r, f) = jit(&a, &mut [v]);
        assert_eq!(r.as_number(), Some(expected));
        assert!(has(&stencils(&f), stencil), "{v:?}: {:?}", stencils(&f));
    }
}

#[test]
fn too_many_recompiles_finish_in_the_interpreter() {
    // 20 independent overflowing adds: each exits once. After 16 compiles,
    // the rest runs in the interpreter.
    let mut a = Assembler::new();
    for r in 0..20 {
        a.lda_smi(i32::MAX).add_smi(1).star(r);
    }
    a.ldar(19).ret();
    let mut regs = [Value::UNDEFINED; 20];
    let (v, f) = jit(&a, &mut regs);
    assert_eq!(v.as_number(), Some(2147483648.0));
    assert!(regs.iter().all(|r| r.as_number() == Some(2147483648.0)));
    assert_eq!((f.stats.compiles, f.stats.exits), (16, 16));
}

// ---- VM registers in machine registers (milestone 3) ----

use toi::jit::{GPR_SLOTS, Home};

fn homes(f: &JitFunction) -> Vec<Home> {
    f.last_code().unwrap().homes.clone()
}

#[test]
fn loop_registers_live_in_machine_registers() {
    let (v, f) = jit(&sum_loop(1000), &mut [Value::UNDEFINED; 3]);
    assert_eq!(v, Value::from_i32(499500));
    assert!(
        homes(&f).iter().all(|h| matches!(h, Home::Gpr(_))),
        "{:?}",
        homes(&f)
    );
    let s = stencils(&f);
    assert!(
        s.iter().all(|n| !n.ends_with("_m")),
        "memory access in {s:?}"
    );
}

#[test]
fn doubles_are_pinned_in_fp_registers() {
    // After the overflow exit, the sum is a double everywhere in the loop,
    // so the recompiled region keeps it unboxed in an FP register.
    let n = 100_000i64;
    let mut regs = [Value::UNDEFINED; 3];
    let (v, f) = jit(&sum_loop(n as i32), &mut regs);
    assert_eq!(v.as_number(), Some((n * (n - 1) / 2) as f64));
    assert!(matches!(homes(&f)[0], Home::Fpr(_)), "{:?}", homes(&f));
    assert!(has(&stencils(&f), "add_fi"), "{:?}", stencils(&f));
    // Spilled back, boxed, at return.
    assert_eq!(regs[0].as_number(), v.as_number());
    assert_eq!(regs[1], Value::from_i32(n as i32));
}

#[test]
fn exits_spill_cached_registers() {
    // r1 is written in a GPR slot, then `add` overflows and exits; the
    // interpreter-visible register file must have r1's new value.
    let mut a = Assembler::new();
    a.lda_smi(7).star(1).lda_smi(i32::MAX).star(0).add(0).ret();
    let mut regs = [Value::UNDEFINED; 2];
    let (v, f) = jit(&a, &mut regs);
    assert_eq!(v.as_number(), Some(2.0 * i32::MAX as f64));
    assert_eq!(regs[1], Value::from_i32(7));
    assert_eq!(f.stats.exits, 1);
}

#[test]
fn more_registers_than_slots_use_memory() {
    // GPR_SLOTS + 3 counters incremented in a loop: the most used get slots,
    // the rest stay in memory, and everything still agrees.
    let k = GPR_SLOTS + 3;
    let i = k as u8;
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0);
    for r in 0..=k as u8 {
        a.star(r);
    }
    a.bind(top);
    for r in 0..k as u8 {
        a.ldar(r).add_smi(r as i32 + 1).star(r);
    }
    a.ldar(i).inc().star(i).jump_if_less_smi(100, top);
    a.ldar(0).ret();
    let mut regs = vec![Value::UNDEFINED; k + 1];
    exec(a.clone(), &mut regs.clone());
    let (v, f) = jit(&a, &mut regs);
    assert_eq!(v, Value::from_i32(100));
    let h = homes(&f);
    assert_eq!(
        h.iter().filter(|h| matches!(h, Home::Gpr(_))).count(),
        GPR_SLOTS
    );
    assert_eq!(
        h.iter().filter(|h| **h == Home::Mem).count(),
        k + 1 - GPR_SLOTS
    );
    for (r, v) in regs.iter().enumerate().take(k) {
        assert_eq!(*v, Value::from_i32(100 * (r as i32 + 1)), "r{r}");
    }
}

#[test]
fn runtime_calls_preserve_slot_registers() {
    // Each iteration calls the runtime (`true + 1` takes the generic slow
    // path) while 8 int counters sit in GPR slots (on arm64 including the
    // caller-saved x0–x6), a double sits pinned in an FP slot, and the
    // accumulator passes through `facc`. The per-region wrapper must
    // preserve all of them.
    let (d, b, t, i) = (8u8, 9u8, 10u8, 11u8);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(i);
    for r in 0..8 {
        a.star(r);
    }
    a.bind(top);
    for r in 0..8u8 {
        a.ldar(r).add_smi(r as i32 + 1).star(r);
    }
    a.ldar(d).add_smi(1).star(d);
    a.ldar(b).add_smi(1).star(t);
    a.ldar(i).inc().star(i).jump_if_less_smi(50, top);
    a.ldar(d).ret();
    let mut regs = vec![Value::UNDEFINED; 12];
    regs[d as usize] = Value::from_f64(0.5);
    regs[b as usize] = Value::TRUE;
    let v = exec(a.clone(), &mut regs.clone());
    assert_eq!(v.as_number(), Some(50.5));
    let (_, f) = jit(&a, &mut regs);
    assert!(
        matches!(homes(&f)[d as usize], Home::Fpr(_)),
        "{:?}",
        homes(&f)
    );
    assert!(has(&stencils(&f), "add_smi"), "{:?}", stencils(&f));
    for (r, v) in regs.iter().enumerate().take(8) {
        assert_eq!(*v, Value::from_i32(50 * (r as i32 + 1)), "r{r}");
    }
    assert_eq!(regs[t as usize], Value::from_i32(2));
}
