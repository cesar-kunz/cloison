//! Experiment: run counting loops through the tail-call dispatcher.
//!
//! ```text
//! int:    i = 0; while (i < n) { t = i + i; i = i + 1; }        return t;
//! double: s = 0; i = 0; while (i < n) { s = s + i; i = i + 1; } return s;
//! ```
//!
//! Both run 8 bytecodes per iteration. In `int` every value stays int32, so
//! every handler takes its fast path. In `double`, `s` leaves the int32 range
//! after ~65k iterations, and from then on `add` takes its slow path.

use std::time::Instant;
use toi::bytecode::Assembler;
use toi::value::Value;

const S: u8 = 0;
const I: u8 = 1;
const LIMIT: u8 = 2;

fn program(n: i32, body: impl FnOnce(&mut Assembler)) -> Vec<u8> {
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(S).star(I);
    a.lda_smi(n).star(LIMIT);
    a.bind(top);
    body(&mut a);
    a.ldar(I).inc().nop().star(I);
    a.jump_if_less(LIMIT, top);
    a.ldar(S).ret();
    a.finish()
}

fn bench(name: &str, n: i32, code: &[u8], expected: f64) {
    let mut regs = [Value::UNDEFINED; 3];
    let start = Instant::now();
    let result = unsafe { toi::interp::run(code, &mut regs) };
    let elapsed = start.elapsed();

    assert_eq!(result.as_number(), Some(expected), "{name}: got {result:?}");
    let insns = n as u64 * 8;
    println!(
        "{name:<6} n={n} result={result:?} time={:.3}s  {:.2} ns/bytecode  {:.0} M bytecodes/s",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / insns as f64,
        insns as f64 / elapsed.as_secs_f64() / 1e6,
    );
}

fn main() {
    let n: i32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000_000);

    let int = program(n, |a| {
        a.ldar(I).add(I).star(S);
    });
    bench("int", n, &int, 2.0 * (n as f64 - 1.0));

    let double = program(n, |a| {
        a.ldar(S).add(I).star(S);
    });
    // Exact: the sum stays below 2^53 for any i32 n.
    bench("double", n, &double, n as f64 * (n as f64 - 1.0) / 2.0);
}
