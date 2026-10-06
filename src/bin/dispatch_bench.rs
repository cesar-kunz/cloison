//! Experiment: run a counting loop through the tail-call dispatcher.
//!
//! ```text
//! sum = 0; i = 0;
//! while (i < n) { sum = sum + i; i = i + 1; }
//! return sum;
//! ```

use std::time::Instant;
use toi::bytecode::Assembler;

fn main() {
    let n: i32 = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(100_000_000);

    let (sum, i, limit) = (0, 1, 2);
    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(sum);
    a.lda_smi(0).star(i);
    a.lda_smi(n).star(limit);
    a.bind(top);
    a.ldar(sum).add(i).star(sum);
    a.ldar(i).inc().nop().star(i);
    a.jump_if_less(limit, top);
    a.ldar(sum).ret();
    let code = a.finish();

    let start = Instant::now();
    let result = unsafe { toi::interp::run(&code, 3) };
    let elapsed = start.elapsed();

    let expected = (n as i64) * (n as i64 - 1) / 2;
    assert_eq!(result, expected);
    let insns = n as u64 * 8;
    println!(
        "n={n} result={result} time={:.3}s  {:.2} ns/bytecode  {:.0} M bytecodes/s",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / insns as f64,
        insns as f64 / elapsed.as_secs_f64() / 1e6,
    );
}
