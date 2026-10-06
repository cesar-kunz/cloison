use toi::bytecode::Assembler;
use toi::interp::run;

fn sum_loop(n: i32) -> Vec<u8> {
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
    a.finish()
}

#[test]
fn straight_line() {
    let mut a = Assembler::new();
    a.lda_smi(40).star(0).lda_smi(2).add(0).ret();
    assert_eq!(unsafe { run(&a.finish(), 1) }, 42);
}

#[test]
fn negative_immediate() {
    let mut a = Assembler::new();
    a.lda_smi(-7).ret();
    assert_eq!(unsafe { run(&a.finish(), 0) }, -7);
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
    assert_eq!(unsafe { run(&a.finish(), 0) }, 99);
}

#[test]
fn loop_does_not_grow_the_stack() {
    // 10M iterations × 8 tail calls each. Without guaranteed TCO this would
    // overflow the stack, in debug builds too.
    let n = 10_000_000i64;
    assert_eq!(unsafe { run(&sum_loop(n as i32), 3) }, n * (n - 1) / 2);
}
