//! Every program runs on both engines, the interpreter and the
//! copy-and-patch JIT, which must agree on the result and on the final
//! register file.

use toi::bytecode::Assembler;
use toi::value::Value;

/// Bitwise equality, except that any two NaNs match (engines may differ
/// in which NaN they produce).
fn same(a: Value, b: Value) -> bool {
    let nan = |v: Value| v.as_number().is_some_and(f64::is_nan);
    a == b || (nan(a) && nan(b))
}

fn exec(a: Assembler, regs: &mut [Value]) -> Value {
    let code = a.finish();
    let mut jit_regs = regs.to_vec();
    let interp = unsafe { toi::interp::run(&code, regs) };
    let jit = unsafe { toi::jit::run(&code, &mut jit_regs) };
    assert!(same(interp, jit), "interp returned {interp:?}, jit {jit:?}");
    for (r, (i, j)) in regs.iter().zip(&jit_regs).enumerate() {
        assert!(same(*i, *j), "r{r}: interp {i:?}, jit {j:?}");
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
