//! Every program runs on seven engines: the interpreter, the eager JIT with
//! generic stencils (milestone 1), with type-specialized stencils
//! (milestone 2), with VM registers in machine registers (milestone 3),
//! with fused stencils, with unboxed int32 in slots, and tiered (starting in
//! the interpreter, tiering up at a loop's third back edge).
//! They must agree on the result and on the final register file.

use cloison::bytecode::{Arg, Assembler, Op};
use cloison::jit::{JitFunction, Tiering};
use cloison::value::Value;

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
    let interp = unsafe { cloison::interp::run(&code, regs) };
    // With superinstructions (see `bytecode::quicken`): the same results.
    let mut quick = code.clone();
    cloison::bytecode::quicken(&mut quick);
    let mut quick_regs = initial.clone();
    let q = unsafe { cloison::interp::run(&quick, &mut quick_regs) };
    assert!(
        same(interp, q),
        "interp returned {interp:?}, quickened {q:?}"
    );
    for (r, (i, j)) in regs.iter().zip(&quick_regs).enumerate() {
        assert!(same(*i, *j), "r{r}: interp {i:?}, quickened {j:?}");
    }
    for (name, mut f) in [
        ("jit1", JitFunction::new(&code).eager().unspecialized()),
        (
            "jit2",
            JitFunction::new(&code).eager().without_register_homes(),
        ),
        ("jit3", JitFunction::new(&code).eager().without_fusion()),
        (
            "jit4",
            JitFunction::new(&code).eager().without_unboxed_ints(),
        ),
        // Starts in the interpreter and tiers up at the third loop back edge,
        // so loops cross into JIT code mid-way.
        (
            "tiered",
            JitFunction::new(&code).tiered(Tiering {
                back_edges: 3,
                calls: 1,
            }),
        ),
        ("jit", JitFunction::new(&code).eager()),
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
    let mut f = JitFunction::new(&code).eager();
    let v = unsafe { f.run(regs) };
    (v, f)
}

fn stencils(f: &JitFunction) -> Vec<String> {
    f.last_code()
        .unwrap()
        .stencils()
        .map(String::from)
        .collect()
}

/// Whether some selected stencil adds with type case `case` (`"fi"`, …),
/// alone (`add_fi_<loc>`) or fused (`addr_fi_<r>_<r2>`).
fn has_add(s: &[String], case: &str) -> bool {
    has(s, &format!("add_{case}")) || s.iter().any(|n| n.starts_with(&format!("addr_{case}_")))
}

/// Whether `suffix` is a location: `m`, `g<k>`, `u<k>` or `f<k>`.
fn is_loc(suffix: &str) -> bool {
    suffix == "m"
        || suffix
            .strip_prefix(['g', 'u', 'f'])
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
    // The add may be fused (`addr_ii_<r>_<r2>`); `inc` isn't, since
    // `sum_loop` puts a `nop` between it and its `star`.
    assert!(
        (has(&s, "add_ii") || s.iter().any(|n| n.starts_with("addr_ii_")))
            && has(&s, "inc_i")
            && has(&s, "jlt_ii"),
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
    assert!(has_add(&s, "fi") || has_add(&s, "ff"), "{s:?}");
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
    assert!(has_add(&stencils(&f), "ff"), "{:?}", stencils(&f));
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

use cloison::jit::{GPR_SLOTS, Home};

fn homes(f: &JitFunction) -> Vec<Home> {
    f.last_code().unwrap().homes.clone()
}

#[test]
fn loop_registers_live_in_machine_registers() {
    let (v, f) = jit(&sum_loop(1000), &mut [Value::UNDEFINED; 3]);
    assert_eq!(v, Value::from_i32(499500));
    // Only ever int32: unboxed in GPR slots.
    assert!(
        homes(&f).iter().all(|h| matches!(h, Home::Int(_))),
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
    assert!(has_add(&stencils(&f), "fi"), "{:?}", stencils(&f));
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
        h.iter()
            .filter(|h| matches!(h, Home::Gpr(_) | Home::Int(_)))
            .count(),
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

// ---- Fused stencils ----

/// `s = s + k; i = i + 1` while `i < n`, with no `nop`s, so the body and the
/// loop tail fuse.
fn fusible_loop(k: i32, n: i32, start: i32) -> Assembler {
    let (s, i) = (0, 1);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(start).star(s).lda_smi(0).star(i);
    a.bind(top);
    a.ldar(s).add_smi(k).star(s);
    a.ldar(i).inc().star(i).jump_if_less_smi(n, top);
    a.ldar(s).ret();
    a
}

#[test]
fn loop_body_and_tail_fuse() {
    let a = fusible_loop(3, 1000, 0);
    assert_eq!(
        exec(a.clone(), &mut [Value::UNDEFINED; 2]),
        Value::from_i32(3000)
    );
    let (_, f) = jit(&a, &mut [Value::UNDEFINED; 2]);
    let s = stencils(&f);
    assert!(s.iter().any(|n| n.starts_with("addi_u")), "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("incjlt_u")), "{s:?}");
}

#[test]
fn fused_add_overflow_exits_and_resumes() {
    // `s` overflows int32 inside the fused `addi`: one exit, with `acc`
    // rematerialized from the slot, then a recompile where `s` is a double.
    let a = fusible_loop(1000, 100, i32::MAX - 50_000);
    let expected = (i32::MAX - 50_000) as f64 + 100_000.0;
    assert_eq!(
        exec(a.clone(), &mut [Value::UNDEFINED; 2]).as_number(),
        Some(expected)
    );
    let (v, f) = jit(&a, &mut [Value::UNDEFINED; 2]);
    assert_eq!(v.as_number(), Some(expected));
    assert_eq!((f.stats.compiles, f.stats.exits), (2, 1));
    assert!(
        stencils(&f).iter().any(|n| n.starts_with("addi_f")),
        "{:?}",
        stencils(&f)
    );
}

#[test]
fn fused_loop_tail_overflow_exits() {
    // A do-while entered with the counter at i32::MAX: the fused `incjlt`
    // overflows on its first run and exits; the double result then fails
    // `< 0`, ending the loop.
    // The start value comes from r1.
    let mut a = Assembler::new();
    let top = a.label();
    a.ldar(1).star(0);
    a.bind(top);
    a.ldar(0).inc().star(0).jump_if_less_smi(0, top);
    a.ldar(0).ret();
    let max = [Value::UNDEFINED, Value::from_i32(i32::MAX)];
    assert_eq!(
        exec(a.clone(), &mut max.clone()).as_number(),
        Some(2147483648.0)
    );
    let code = a.clone().finish();
    let mut f = JitFunction::new(&code).eager();
    let v = unsafe { f.run(&mut max.clone()) };
    assert_eq!(v.as_number(), Some(2147483648.0));
    assert_eq!(f.stats.exits, 1);
    // Without the overflow, the only compile shows the fused tail.
    let mut first = JitFunction::new(&code).eager();
    unsafe { first.run(&mut [Value::UNDEFINED, Value::from_i32(-5)]) };
    assert_eq!(first.stats.exits, 0);
    let s = stencils(&first);
    assert!(s.iter().any(|n| n.starts_with("incjlt_u")), "{s:?}");
}

#[test]
fn live_accumulator_after_a_fused_group() {
    // After `ldar r0; add_smi 5; star r0`, acc is still read by `star r1`
    // and `add r0`, so it must be rematerialized.
    let mut a = Assembler::new();
    a.lda_smi(1).star(0).star(1);
    a.ldar(0).add_smi(5).star(0).star(1).add(0).ret();
    assert_eq!(
        exec(a.clone(), &mut [Value::UNDEFINED; 2]),
        Value::from_i32(12)
    );
    let (_, f) = jit(&a, &mut [Value::UNDEFINED; 2]);
    assert!(
        stencils(&f).iter().any(|n| n.starts_with("addi_u")),
        "{:?}",
        stencils(&f)
    );
}

#[test]
fn no_fusion_across_a_jump_target() {
    // A jump lands on the `add_smi` in the middle of `ldar; add_smi; star`.
    let mut a = Assembler::new();
    let (mid, out) = (a.label(), a.label());
    a.lda_smi(10).star(0).lda_smi(20).jump(mid);
    a.ldar(0);
    a.bind(mid);
    a.add_smi(1).star(0).jump(out);
    a.bind(out);
    a.ldar(0).ret();
    assert_eq!(
        exec(a.clone(), &mut [Value::UNDEFINED]),
        Value::from_i32(21)
    );
    let (_, f) = jit(&a, &mut [Value::UNDEFINED]);
    assert!(
        !stencils(&f).iter().any(|n| n.starts_with("addi_")),
        "{:?}",
        stencils(&f)
    );
}

#[test]
fn fused_double_accumulation() {
    // `d = d + i` with `d` pinned in an FP slot and `i` an int32 in a GPR
    // slot: fuses to `addr_fi`.
    let (d, i) = (0, 1);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(i);
    a.bind(top);
    a.ldar(d).add(i).star(d);
    a.ldar(i).inc().star(i).jump_if_less_smi(100, top);
    a.ldar(d).ret();
    let regs = [Value::from_f64(0.5), Value::UNDEFINED];
    assert_eq!(exec(a.clone(), &mut regs.clone()).as_number(), Some(4950.5));
    let (_, f) = jit(&a, &mut regs.clone());
    assert!(
        stencils(&f).iter().any(|n| n.starts_with("addr_fi_f")),
        "{:?}",
        stencils(&f)
    );
}

// ---- Unboxed int32 slots ----

#[test]
fn fused_register_add_overflow_undoes_and_exits() {
    // `a = a + b` in place on unboxed slots overflows: the slot holds the
    // wrapped sum until the exit stub undoes it.
    let (a_, b_, i) = (0, 1, 2);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(i);
    a.bind(top);
    a.ldar(a_).add(b_).star(a_);
    a.ldar(i).inc().star(i).jump_if_less_smi(10, top);
    a.ldar(a_).ret();
    let regs = [
        Value::from_i32(i32::MAX - 5_000_000),
        Value::from_i32(1_000_000),
        Value::UNDEFINED,
    ];
    let v = exec(a.clone(), &mut regs.clone());
    assert_eq!(v.as_number(), Some((i32::MAX - 5_000_000) as f64 + 10e6));
    let code = a.finish();
    let mut f = JitFunction::new(&code).eager();
    let mut r = regs;
    assert_eq!(unsafe { f.run(&mut r) }.as_number(), v.as_number());
    assert_eq!(f.stats.exits, 1);
}

#[test]
fn registers_written_on_some_paths_stay_boxed() {
    // r0 isn't live at entry and is only written when r1 >= 0, so it isn't
    // definitely assigned at `return`: it must not be pinned, or the spill
    // at `return` would clobber its untouched value (null) when r1 < 0.
    let mut a = Assembler::new();
    let (no, out) = (a.label(), a.label());
    a.ldar(1).jump_if_less_smi(0, no);
    a.lda_smi(5).star(0).jump(out);
    a.bind(no);
    a.lda_smi(9);
    a.bind(out);
    a.ret();
    for start in [Value::from_i32(-1), Value::from_i32(1)] {
        let regs = [Value::NULL, start];
        exec(a.clone(), &mut regs.clone());
        let (_, f) = jit(&a, &mut regs.clone());
        assert!(!matches!(homes(&f)[0], Home::Int(_)), "{:?}", homes(&f));
    }
}

// ---- Unboxed int32 accumulator ----

#[test]
fn int_acc_is_boxed_where_a_join_widens_it() {
    // At `out`, acc is int32 from one path and `null` from the other, so the
    // join is `Any`: the int path's edge must box acc. (A raw 7 would read
    // as the value with bits 0x7, which is `true`.)
    let mut a = Assembler::new();
    let (b, out) = (a.label(), a.label());
    a.ldar(0).jump_if_less_smi(0, b);
    a.lda_smi(7).jump(out);
    a.bind(b);
    a.ldar(1);
    a.bind(out);
    a.star(2).ret();
    for (start, expected) in [(1, Value::from_i32(7)), (-1, Value::NULL)] {
        let regs = [Value::from_i32(start), Value::NULL, Value::UNDEFINED];
        let mut r = regs;
        assert_eq!(exec(a.clone(), &mut r), expected);
        assert_eq!(r[2], expected);
    }
}

// ---- Three-address and chained adds ----

/// Names of the fused groups (and in-place adds) in `f`'s last code.
fn group_names(f: &JitFunction) -> Vec<String> {
    stencils(f)
        .into_iter()
        .filter(|n| n.contains('+') || n.starts_with("addr_"))
        .collect()
}

/// `r2 = r0 + r1 (+ r1 …)` in a loop, `extra` more adds of r1.
fn three_address_loop(extra: usize) -> Assembler {
    let (x, y, t, i) = (0, 1, 2, 3);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(i).star(t);
    a.bind(top);
    a.ldar(x).add(y);
    for _ in 0..extra {
        a.add(y);
    }
    a.star(t);
    a.ldar(i).inc().star(i).jump_if_less_smi(10, top);
    a.ldar(t).ret();
    a
}

#[test]
fn three_address_add_fuses_as_move_and_add() {
    let a = three_address_loop(0);
    let regs = [
        Value::from_i32(5),
        Value::from_i32(7),
        Value::UNDEFINED,
        Value::UNDEFINED,
    ];
    assert_eq!(exec(a.clone(), &mut regs.clone()), Value::from_i32(12));
    let (_, f) = jit(&a, &mut regs.clone());
    let g = group_names(&f);
    assert!(
        g.iter()
            .any(|n| n.starts_with("mov_u") && n.contains("+addr_ii_u")),
        "{g:?}"
    );
}

#[test]
fn add_chain_fuses() {
    // t = x + y + y + y
    let a = three_address_loop(2);
    let regs = [
        Value::from_i32(5),
        Value::from_i32(7),
        Value::UNDEFINED,
        Value::UNDEFINED,
    ];
    assert_eq!(exec(a.clone(), &mut regs.clone()), Value::from_i32(26));
    let (_, f) = jit(&a, &mut regs.clone());
    let g = group_names(&f);
    assert!(
        g.iter().any(|n| n.matches("+addr_ii_u").count() == 3),
        "{g:?}"
    );
}

#[test]
fn add_chain_overflow_mid_chain() {
    // t = x + y + y in straight-line code: x + y fits, the second add
    // overflows. The exit must undo it and resume with acc = x + y. (In a
    // loop recomputing t every iteration, a wrong value would be
    // overwritten and go unnoticed.) t is written first so it can be pinned
    // in a slot (see `pinnable`); otherwise it stays boxed and the chain
    // doesn't fuse.
    let (x, y, t) = (0, 1, 2);
    let mut a = Assembler::new();
    a.lda_smi(0).star(t);
    a.ldar(x).add(y).add(y).star(t).ldar(t).ret();
    let yv = 1_500_000_000;
    let regs = [Value::from_i32(7), Value::from_i32(yv), Value::UNDEFINED];
    let expected = 7.0 + 2.0 * yv as f64;
    assert_eq!(
        exec(a.clone(), &mut regs.clone()).as_number(),
        Some(expected)
    );
    let mut r = regs;
    let (v, f) = jit(&a, &mut r);
    assert_eq!(v.as_number(), Some(expected));
    assert_eq!(f.stats.exits, 1);
    assert_eq!(r[t as usize].as_number(), Some(expected));
    // Without the overflow, the code shows the fused chain.
    let (_, f) = jit(
        &a,
        &mut [Value::from_i32(1), Value::from_i32(2), Value::UNDEFINED],
    );
    let g = group_names(&f);
    assert!(
        g.iter().any(|n| n.matches("+addr_ii_u").count() == 2),
        "{g:?}"
    );
}

#[test]
fn add_chain_reading_its_destination_is_not_fused() {
    // t = x + t reads t's old value: must not become `t = x; t += t`.
    let (x, t, i) = (0, 1, 2);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(i).star(t);
    a.bind(top);
    a.ldar(x).add(t).star(t);
    a.ldar(i).inc().star(i).jump_if_less_smi(10, top);
    a.ldar(t).ret();
    let regs = [Value::from_i32(3), Value::UNDEFINED, Value::UNDEFINED];
    assert_eq!(exec(a.clone(), &mut regs.clone()), Value::from_i32(30));
    let (_, f) = jit(&a, &mut regs.clone());
    assert!(
        !stencils(&f).iter().any(|n| n.starts_with("mov_u")),
        "{:?}",
        stencils(&f)
    );
}

#[test]
fn three_address_double_from_int() {
    // s = i + d with i int32 and d, s doubles: mov_f<s>_u<i> + addr_ff.
    let (d, s, i) = (0, 1, 2);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(i);
    a.bind(top);
    a.ldar(i).add(d).star(s);
    a.ldar(i).inc().star(i).jump_if_less_smi(10, top);
    a.ldar(s).ret();
    let regs = [Value::from_f64(0.5), Value::from_f64(0.0), Value::UNDEFINED];
    assert_eq!(exec(a.clone(), &mut regs.clone()).as_number(), Some(9.5));
    let (_, f) = jit(&a, &mut regs.clone());
    assert!(
        stencils(&f)
            .iter()
            .any(|n| n.starts_with("mov_f") && n.contains("+addr_ff_f")),
        "{:?}",
        stencils(&f)
    );
}

// ---- Tiering and versions (milestone 4) ----

fn tiered(back_edges: i64, calls: u32) -> Tiering {
    Tiering { back_edges, calls }
}

#[test]
fn hot_loop_tiers_up_at_its_header() {
    let code = sum_loop(1000).finish();
    let mut f = JitFunction::new(&code).tiered(tiered(100, 10));
    let v = unsafe { f.run(&mut [Value::UNDEFINED; 3]) };
    assert_eq!(v, Value::from_i32(499500));
    assert_eq!(
        (f.stats.interp_runs, f.stats.osr_entries, f.stats.compiles),
        (1, 1, 1)
    );
}

#[test]
fn short_loop_never_compiles() {
    let code = sum_loop(10).finish();
    let mut f = JitFunction::new(&code).tiered(tiered(100, 10));
    let v = unsafe { f.run(&mut [Value::UNDEFINED; 3]) };
    assert_eq!(v, Value::from_i32(45));
    assert_eq!((f.stats.osr_entries, f.stats.compiles), (0, 0));
}

#[test]
fn back_edge_budget_persists_across_calls() {
    // 30 iterations per call, budget 100: the fourth call tiers up mid-loop.
    let code = sum_loop(30).finish();
    let mut f = JitFunction::new(&code).tiered(tiered(100, 1000));
    for call in 1..=5 {
        let v = unsafe { f.run(&mut [Value::UNDEFINED; 3]) };
        assert_eq!(v, Value::from_i32(435));
        let expected = if call < 4 { 0 } else { 1 };
        assert_eq!(f.stats.osr_entries, expected, "call {call}");
    }
    // From the fourth call on, the function is hot and starts in JIT code.
    assert_eq!(f.stats.interp_runs, 4);
}

#[test]
fn frequent_calls_compile_at_entry() {
    let mut a = Assembler::new();
    a.ldar(0).add_smi(1).ret();
    let code = a.finish();
    let mut f = JitFunction::new(&code).tiered(tiered(100, 3));
    for _ in 0..10 {
        assert_eq!(
            unsafe { f.run(&mut [Value::from_i32(41)]) },
            Value::from_i32(42)
        );
    }
    assert_eq!((f.stats.interp_runs, f.stats.compiles), (3, 1));
}

#[test]
fn versions_are_reused_across_calls() {
    let mut a = Assembler::new();
    a.ldar(0).add_smi(1).ret();
    let code = a.finish();
    let mut f = JitFunction::new(&code).eager();
    for i in 0..10 {
        let (arg, expected) = if i % 2 == 0 {
            (Value::from_i32(1), 2.0)
        } else {
            (Value::from_f64(1.5), 2.5)
        };
        assert_eq!(unsafe { f.run(&mut [arg]) }.as_number(), Some(expected));
    }
    assert_eq!((f.stats.compiles, f.version_count()), (2, 2));
}

#[test]
fn an_int_enters_a_double_version() {
    // Compiled for a double first: a later int32 fits, converted to double.
    let mut a = Assembler::new();
    a.ldar(0).add_smi(1).ret();
    let code = a.finish();
    let mut f = JitFunction::new(&code).eager();
    assert_eq!(
        unsafe { f.run(&mut [Value::from_f64(0.5)]) }.as_number(),
        Some(1.5)
    );
    assert_eq!(
        unsafe { f.run(&mut [Value::from_i32(2)]) }.as_number(),
        Some(3.0)
    );
    assert_eq!(f.stats.compiles, 1);
}

#[test]
fn dead_registers_dont_split_versions() {
    // r0 is overwritten before it is read: its entry type is irrelevant.
    let mut a = Assembler::new();
    a.lda_smi(5).star(0).ldar(0).add_smi(1).ret();
    let code = a.finish();
    let mut f = JitFunction::new(&code).eager();
    for arg in [Value::from_i32(1), Value::from_f64(1.5), Value::NULL] {
        assert_eq!(unsafe { f.run(&mut [arg]) }, Value::from_i32(6));
    }
    assert_eq!(f.stats.compiles, 1);
}

#[test]
fn megamorphic_entries_generalize() {
    // Five entry type states, none fitting an earlier version: after four
    // versions, one generalized version (any, any) replaces them.
    let mut a = Assembler::new();
    a.ldar(0).add(1).ret();
    let code = a.finish();
    let mut f = JitFunction::new(&code).eager();
    let (i, d, n) = (Value::from_i32(1), Value::from_f64(0.5), Value::NULL);
    let calls = [
        (i, i),
        (d, i),
        (i, d),
        (n, i),
        (i, n),
        (d, d),
        (n, n),
        (i, i),
    ];
    let expected = [2.0, 1.5, 1.5, 1.0, 1.0, 1.0, 0.0, 2.0];
    for ((x, y), e) in calls.into_iter().zip(expected) {
        assert_eq!(
            unsafe { f.run(&mut [x, y]) }.as_number(),
            Some(e),
            "{x:?} + {y:?}"
        );
    }
    assert_eq!(
        (f.stats.compiles, f.stats.generalizations, f.version_count()),
        (5, 1, 1)
    );
}

#[test]
fn accumulator_carried_across_the_back_edge() {
    // acc is live at the loop header (`add_smi` reads it), so tiering up
    // there must carry it over.
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0);
    a.bind(top);
    a.add_smi(1).star(0).jump_if_less_smi(100, top);
    a.ret();
    assert_eq!(
        exec(a.clone(), &mut [Value::UNDEFINED]),
        Value::from_i32(100)
    );
    let code = a.finish();
    let mut f = JitFunction::new(&code).tiered(tiered(10, 10));
    assert_eq!(
        unsafe { f.run(&mut [Value::UNDEFINED]) },
        Value::from_i32(100)
    );
    assert_eq!(f.stats.osr_entries, 1);
}

#[test]
fn tiering_is_the_default() {
    // Short: stays in the interpreter. Long: tiers up after 1000 back edges.
    let mut short = JitFunction::new(&sum_loop(100).finish());
    assert_eq!(
        unsafe { short.run(&mut [Value::UNDEFINED; 3]) },
        Value::from_i32(4950)
    );
    assert_eq!((short.stats.compiles, short.stats.osr_entries), (0, 0));
    let mut long = JitFunction::new(&sum_loop(5000).finish());
    let n = 5000i64;
    assert_eq!(
        unsafe { long.run(&mut [Value::UNDEFINED; 3]) }.as_number(),
        Some((n * (n - 1) / 2) as f64)
    );
    assert_eq!(long.stats.osr_entries, 1);
}

// ---- Numeric operations, comparisons and boolean jumps ----

fn emit(a: &mut Assembler, op: Op, args: &[Arg]) {
    a.emit(op, args);
}

/// Values of every kind the numeric operations distinguish.
fn sample_values() -> Vec<Value> {
    let mut v: Vec<Value> = [0, 1, -1, 7, -6, -7, 1024, i32::MIN, i32::MAX]
        .into_iter()
        .map(Value::from_i32)
        .collect();
    v.extend(
        [
            2.5,
            -0.0,
            -6.0,
            7.75,
            f64::NAN,
            f64::INFINITY,
            -1e20,
            1e300,
            4294967301.0,
        ]
        .into_iter()
        .map(Value::from_f64),
    );
    v.extend([Value::UNDEFINED, Value::NULL, Value::TRUE, Value::FALSE]);
    v
}

#[test]
fn immediate_operations_agree_on_every_kind_of_value() {
    // `ldar r0; <op> k; return`, entered with each value in r0: the JIT
    // specializes on its type (int32, double or any).
    let ops = [Op::ModSmi, Op::BitAndSmi, Op::BitOrSmi, Op::TestEqualSmi];
    for op in ops {
        for k in [3, 8, 1, -3, 0, 7, i32::MIN] {
            for v in sample_values() {
                let mut a = Assembler::new();
                a.ldar(0);
                emit(&mut a, op, &[Arg::Imm(k)]);
                a.ret();
                exec(a, &mut [v]);
            }
        }
    }
}

#[test]
fn comparisons_agree_on_every_pair_of_values() {
    for op in [Op::TestLessThan, Op::TestEqual] {
        for l in sample_values() {
            for r in sample_values() {
                // As a value, and fused with each conditional jump.
                let mut a = Assembler::new();
                a.ldar(1);
                emit(&mut a, op, &[Arg::Reg(0)]);
                a.ret();
                exec(a, &mut [l, r]);
                for jump in [Op::JumpIfTrue, Op::JumpIfFalse] {
                    let mut a = Assembler::new();
                    let yes = a.label();
                    a.ldar(1);
                    emit(&mut a, op, &[Arg::Reg(0)]);
                    emit(&mut a, jump, &[Arg::Label(yes)]);
                    a.lda_smi(0).ret();
                    a.bind(yes);
                    a.lda_smi(1).ret();
                    exec(a, &mut [l, r]);
                }
            }
        }
    }
}

#[test]
fn to_boolean_jumps_agree_on_every_kind_of_value() {
    for jump in [
        Op::JumpIfToBooleanTrue,
        Op::JumpIfToBooleanFalse,
        Op::JumpIfTrue,
        Op::JumpIfFalse,
    ] {
        for v in sample_values() {
            let mut a = Assembler::new();
            let yes = a.label();
            a.ldar(0);
            emit(&mut a, jump, &[Arg::Label(yes)]);
            a.lda_smi(0).ret();
            a.bind(yes);
            a.lda_smi(1).ret();
            exec(a, &mut [v]);
        }
    }
}

/// A loop over `i` in `0..n` using every numeric operation, with each
/// comparison followed by the jump that consumes it:
///
/// ```text
/// a = i % 7; b = i & 15
/// if (b < a) n += 1
/// if ((i & 3) == 0) n += 2
/// if (a == b) n += 4
/// if (b == 5) n = (n | 1) + 8
/// ```
fn numeric_loop(n: i32, modulus: i32) -> Assembler {
    let (i, cnt, ra, rb) = (0u8, 1u8, 2u8, 3u8);
    let mut a = Assembler::new();
    let top = a.label();
    let (s1, s2, s3, s4) = (a.label(), a.label(), a.label(), a.label());
    a.lda_smi(0).star(i).star(cnt);
    a.bind(top);
    a.ldar(i);
    emit(&mut a, Op::ModSmi, &[Arg::Imm(modulus)]);
    a.star(ra);
    a.ldar(i);
    emit(&mut a, Op::BitAndSmi, &[Arg::Imm(15)]);
    a.star(rb);
    a.ldar(ra);
    emit(&mut a, Op::TestLessThan, &[Arg::Reg(rb)]);
    emit(&mut a, Op::JumpIfFalse, &[Arg::Label(s1)]);
    a.ldar(cnt).inc().star(cnt);
    a.bind(s1);
    a.ldar(i);
    emit(&mut a, Op::BitAndSmi, &[Arg::Imm(3)]);
    emit(&mut a, Op::TestEqualSmi, &[Arg::Imm(0)]);
    emit(&mut a, Op::JumpIfFalse, &[Arg::Label(s2)]);
    a.ldar(cnt).add_smi(2).star(cnt);
    a.bind(s2);
    a.ldar(rb);
    emit(&mut a, Op::TestEqual, &[Arg::Reg(ra)]);
    emit(&mut a, Op::JumpIfFalse, &[Arg::Label(s3)]);
    a.ldar(cnt).add_smi(4).star(cnt);
    a.bind(s3);
    a.ldar(rb);
    emit(&mut a, Op::TestEqualSmi, &[Arg::Imm(5)]);
    emit(&mut a, Op::JumpIfFalse, &[Arg::Label(s4)]);
    a.ldar(cnt);
    emit(&mut a, Op::BitOrSmi, &[Arg::Imm(1)]);
    a.add_smi(8).star(cnt);
    a.bind(s4);
    a.ldar(i).inc().star(i).jump_if_less_smi(n, top);
    a.ldar(cnt).ret();
    a
}

#[test]
fn numeric_loop_fuses_compares_with_branches() {
    for modulus in [7, 8] {
        let a = numeric_loop(1000, modulus);
        let v = exec(a.clone(), &mut [Value::UNDEFINED; 4]);
        let (w, f) = jit(&a, &mut [Value::UNDEFINED; 4]);
        assert_eq!(v, w);
        assert_eq!((f.stats.compiles, f.stats.exits), (1, 0));
        let s = stencils(&f);
        // `i` never holds a negative int32: the `_u` forms, without sign
        // handling (7's magic number shift is 31 + ⌈log2 7⌉).
        let mod_stencil = if modulus == 8 {
            "mod_smi_pow2_u"
        } else {
            "mod_smi_u_s34"
        };
        for want in [mod_stencil, "bit_and_smi_i", "bit_or_smi_i"] {
            assert!(
                s.iter()
                    .any(|n| n == want || n.starts_with(&format!("{want}_k16"))),
                "{want}: {s:?}"
            );
        }
        for want in ["tlt_jf_", "tstz_jf_", "teq_jf_", "teq_smi_jf_"] {
            assert!(s.iter().any(|n| n.starts_with(want)), "{want}: {s:?}");
        }
        assert!(
            !s.iter().any(|n| n.starts_with("test_") || n == "jfalse"),
            "unfused comparison in {s:?}"
        );
    }
}

#[test]
fn a_live_boolean_is_not_fused_away() {
    // The comparison's boolean is still read after the jump (returned), so
    // the pair can't become a bare compare-and-branch.
    let mut a = Assembler::new();
    let out = a.label();
    a.lda_smi(3).star(0).lda_smi(5);
    emit(&mut a, Op::TestLessThan, &[Arg::Reg(0)]);
    emit(&mut a, Op::JumpIfFalse, &[Arg::Label(out)]);
    a.nop();
    a.bind(out);
    a.ret();
    assert_eq!(exec(a.clone(), &mut [Value::UNDEFINED]), Value::TRUE);
    let (_, f) = jit(&a, &mut [Value::UNDEFINED]);
    assert!(has(&stencils(&f), "test_lt_ii"), "{:?}", stencils(&f));
}

#[test]
fn negative_zero_remainder_exits_then_produces_doubles() {
    // `r = i % k` for i from -9 up: the first zero remainder of a negative
    // dividend (i = -9 for 3, -8 for 4) is -0, a double. One exit; then
    // the recompiled loop computes remainders as doubles.
    for k in [3, 4] {
        let (i, r, sum) = (0u8, 1u8, 2u8);
        let mut a = Assembler::new();
        let top = a.label();
        a.lda_smi(-9).star(i).lda_smi(0).star(sum);
        a.bind(top);
        a.ldar(i);
        emit(&mut a, Op::ModSmi, &[Arg::Imm(k)]);
        a.star(r).ldar(sum).add(r).star(sum);
        a.ldar(i).inc().star(i).jump_if_less_smi(10, top);
        a.ldar(r).ret();
        let mut regs = [Value::UNDEFINED; 3];
        let v = exec(a.clone(), &mut regs);
        assert_eq!(v.as_number(), Some(9.0 % k as f64));
        let (_, f) = jit(&a, &mut [Value::UNDEFINED; 3]);
        assert_eq!(f.stats.exits, 1, "k = {k}");
        let s = stencils(&f);
        assert!(
            s.iter()
                .any(|n| n.starts_with("mod_smi") && n.contains("iif")),
            "{s:?}"
        );
    }
}

/// `r % 7` for `r` from -40 up, where -40 comes from outside (the
/// register's value at entry): `r` may be negative, so the remainder
/// handles the sign (`mod_smi_i_*`, then doubles after -0).
#[test]
fn remainders_of_registers_from_outside() {
    let (r, x, sum) = (0u8, 1u8, 2u8);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(sum);
    a.bind(top);
    a.ldar(r);
    emit(&mut a, Op::ModSmi, &[Arg::Imm(7)]);
    a.star(x).ldar(sum).add(x).star(sum);
    a.ldar(r).inc().star(r).jump_if_less_smi(40, top);
    a.ldar(sum).ret();
    let regs = || [Value::from_i32(-40), Value::UNDEFINED, Value::UNDEFINED];
    let v = exec(a.clone(), &mut regs());
    let expect: i32 = (-40..40).map(|r| r % 7).sum();
    assert_eq!(v.as_number(), Some(expect as f64));
    let (_, f) = jit(&a, &mut regs());
    let s = stencils(&f);
    assert!(!s.iter().any(|n| n.starts_with("mod_smi_u")), "{s:?}");
}

/// `i` counts up from 0, so `i & 255` and `(i & 255) + 3` aren't negative:
/// their remainders skip the sign handling (`_u`). `j` is `i`, but -3 when
/// `i == 5`: its remainder handles the sign (`_i`).
#[test]
fn remainders_of_non_negative_values_skip_sign_handling() {
    let (i, j, x, sum) = (0u8, 1u8, 2u8, 3u8);
    let mut a = Assembler::new();
    let (top, other, join) = (a.label(), a.label(), a.label());
    a.lda_smi(0).star(i).star(sum);
    a.bind(top);
    a.ldar(i);
    emit(&mut a, Op::BitAndSmi, &[Arg::Imm(255)]);
    a.add_smi(3);
    emit(&mut a, Op::ModSmi, &[Arg::Imm(10)]);
    a.star(x).ldar(sum).add(x).star(sum);
    a.ldar(i);
    emit(&mut a, Op::TestEqualSmi, &[Arg::Imm(5)]);
    emit(&mut a, Op::JumpIfFalse, &[Arg::Label(other)]);
    a.lda_smi(-3).star(j);
    a.jump(join);
    a.bind(other);
    a.ldar(i).star(j);
    a.bind(join);
    a.ldar(j);
    emit(&mut a, Op::ModSmi, &[Arg::Imm(7)]);
    a.star(x).ldar(sum).add(x).star(sum);
    a.ldar(i).inc().star(i).jump_if_less_smi(1000, top);
    a.ldar(sum).ret();
    let v = exec(a.clone(), &mut [Value::UNDEFINED; 4]);
    let expect: i32 = (0..1000)
        .map(|i| ((i & 255) + 3) % 10 + if i == 5 { -3 } else { i % 7 })
        .sum();
    assert_eq!(v, Value::from_i32(expect));
    let (_, f) = jit(&a, &mut [Value::UNDEFINED; 4]);
    let s = stencils(&f);
    assert!(s.iter().any(|n| n.starts_with("mod_smi_u_s35")), "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("mod_smi_i_s34")), "{s:?}");
}

/// Two values that may be negative though they come from a counter that
/// isn't: `i + -5`, and `acc` at a join reached both from `Ldar i` and
/// from a jump with -3 in it. Their remainders handle the sign.
#[test]
fn remainders_of_negative_adds_and_joins() {
    let (i, x, sum) = (0u8, 1u8, 2u8);
    let mut a = Assembler::new();
    let (top, minus, rem) = (a.label(), a.label(), a.label());
    a.lda_smi(0).star(i).star(sum);
    a.bind(top);
    a.ldar(i).add_smi(-5);
    emit(&mut a, Op::ModSmi, &[Arg::Imm(7)]);
    a.star(x).ldar(sum).add(x).star(sum);
    a.ldar(i);
    emit(&mut a, Op::TestEqualSmi, &[Arg::Imm(3)]);
    emit(&mut a, Op::JumpIfTrue, &[Arg::Label(minus)]);
    a.ldar(i);
    a.bind(rem);
    emit(&mut a, Op::ModSmi, &[Arg::Imm(10)]);
    a.star(x).ldar(sum).add(x).star(sum);
    a.ldar(i).inc().star(i).jump_if_less_smi(1000, top);
    a.ldar(sum).ret();
    a.bind(minus);
    a.lda_smi(-3).jump(rem);
    let v = exec(a.clone(), &mut [Value::UNDEFINED; 3]);
    let expect: i32 = (0..1000)
        .map(|i| (i - 5) % 7 + if i == 3 { -3 } else { i % 10 })
        .sum();
    assert_eq!(v, Value::from_i32(expect));
}

#[test]
fn comparisons_with_untyped_operands_use_generic_stencils() {
    // r0 is undefined, r1 an int32 that is compared against it, fused.
    let mut a = Assembler::new();
    let yes = a.label();
    a.ldar(1);
    emit(&mut a, Op::TestLessThan, &[Arg::Reg(0)]);
    emit(&mut a, Op::JumpIfTrue, &[Arg::Label(yes)]);
    a.lda_smi(0).ret();
    a.bind(yes);
    a.lda_smi(1).ret();
    let regs = [Value::UNDEFINED, Value::from_i32(4)];
    assert_eq!(exec(a.clone(), &mut regs.clone()), Value::from_i32(0));
    let (_, f) = jit(&a, &mut regs.clone());
    let s = stencils(&f);
    assert!(s.iter().any(|n| n.contains("box_iacc+tlt_jt_")), "{s:?}");
}

#[test]
fn add_feeding_to_int32_wraps_without_exits() {
    // s = (s + i) | 0 and t = (t + 1000000007) & -1: the sums leave the
    // int32 range, and ToInt32 wraps them back. Compiled, the adds wrap in
    // 32 bits and never exit.
    let (s, t, i) = (0u8, 1u8, 2u8);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(s).star(t).star(i);
    a.bind(top);
    a.ldar(i).add(s);
    emit(&mut a, Op::BitOrSmi, &[Arg::Imm(0)]);
    a.star(s);
    a.ldar(t).add_smi(1_000_000_007);
    emit(&mut a, Op::BitAndSmi, &[Arg::Imm(-1)]);
    a.star(t);
    a.ldar(i).inc().star(i).jump_if_less_smi(100_000, top);
    a.ldar(s).add(t).ret();
    let v = exec(a.clone(), &mut [Value::UNDEFINED; 3]);
    let (mut ws, mut wt) = (0i32, 0i32);
    for k in 0..100_000 {
        ws = ws.wrapping_add(k);
        wt = wt.wrapping_add(1_000_000_007);
    }
    assert_eq!(v.as_number(), Some(ws as f64 + wt as f64));
    let (_, f) = jit(&a, &mut [Value::UNDEFINED; 3]);
    assert_eq!(f.stats.exits, 0);
    let s = stencils(&f);
    assert!(
        has(&s, "add_wrap_ii") && s.iter().any(|n| n.starts_with("add_smi_wrap_i")),
        "{s:?}"
    );
}
