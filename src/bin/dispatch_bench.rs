//! Experiment: run counting loops through the tail-call dispatcher.
//!
//! ```text
//! dispatch_bench <program> <codegen> [n]
//!
//! program:
//!   int     i = 0; while (i < n) { t = i + i; i = i + 1 }          return t
//!   const   i = 0; while (i < n) { s = s + 3; i = i + 1 }          return s
//!   chain   i = 0; while (i < n) { t = i + i + i + i; i = i + 1 }  return t
//!   double  i = 0; while (i < n) { s = s + i; i = i + 1 }          return s
//!
//! codegen:
//!   generic  constants in registers: `add r`, `jump_if_less r`
//!   smi      constants as immediates: `add_smi 3`, `jump_if_less_smi n`
//! ```
//!
//! `n` is baked into the bytecode, as `i < 100000000` would be. In `double`,
//! `s` leaves the int32 range after ~65k iterations and `add` takes its slow
//! path from then on. `scripts/bench.sh` runs the whole matrix under `perf`.

use std::process::exit;
use std::time::Instant;
use toi::bytecode::Assembler;
use toi::value::Value;

const S: u8 = 0;
const I: u8 = 1;
const LIMIT: u8 = 2;
const K: u8 = 3;

fn usage() -> ! {
    eprintln!("usage: dispatch_bench <int|const|chain|double> <generic|smi> [n]");
    exit(2)
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let (Some(program), Some(codegen)) = (args.get(1), args.get(2)) else {
        usage()
    };
    let smi = match codegen.as_str() {
        "generic" => false,
        "smi" => true,
        _ => usage(),
    };
    let n: i32 = args
        .get(3)
        .map_or(100_000_000, |s| s.parse().unwrap_or_else(|_| usage()));
    assert!(n > 0, "the loop tests at the bottom, so n must be positive");
    let nf = n as f64;

    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(S).star(I);
    a.lda_smi(n).star(LIMIT);
    a.lda_smi(3).star(K);
    a.bind(top);
    let expected = match program.as_str() {
        "int" => {
            a.ldar(I).add(I).star(S);
            2.0 * (nf - 1.0)
        }
        "const" => {
            a.ldar(S);
            if smi {
                a.add_smi(3)
            } else {
                a.add(K)
            };
            a.star(S);
            3.0 * nf
        }
        "chain" => {
            a.ldar(I).add(I).add(I).add(I).star(S);
            4.0 * (nf - 1.0)
        }
        "double" => {
            a.ldar(S).add(I).star(S);
            nf * (nf - 1.0) / 2.0
        }
        _ => usage(),
    };
    a.ldar(I).inc().star(I);
    if smi {
        a.jump_if_less_smi(n, top);
    } else {
        a.jump_if_less(LIMIT, top);
    }
    a.ldar(S).ret();
    let code = a.finish();

    let mut regs = [Value::UNDEFINED; 4];
    let start = Instant::now();
    let result = unsafe { toi::interp::run(&code, &mut regs) };
    let elapsed = start.elapsed();

    assert_eq!(
        result.as_number(),
        Some(expected),
        "{program}/{codegen}: got {result:?}"
    );
    println!(
        "{program}/{codegen} n={n} result={result:?} time={:.3}s {:.2} ns/iter",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / nf,
    );
}
