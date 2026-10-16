//! Experiment: run counting loops through the tail-call dispatcher.
//!
//! ```text
//! dispatch_bench <program> <codegen> [n] [engine]
//!
//! program:
//!   int     i = 0; while (i < n) { t = i + i; i = i + 1 }          return t
//!   const   i = 0; while (i < n) { s = s + 3; i = i + 1 }          return s
//!   chain   i = 0; while (i < n) { t = i + i + i + i; i = i + 1 }  return t
//!   double  i = 0; while (i < n) { s = s + i; i = i + 1 }          return s
//!   wide    i = 0; while (i < n) { a1 = a1 + 1; … a8 = a8 + 8; i = i + 1 }
//!           return a8: 12 live VM registers, more than x86-64's 6 GPR slots
//!   cmp     i = 0; while (i < n) { a = i % 7; b = i & 15;
//!                                  if (b < a) s++; if ((i & 3) == 0) s += 2;
//!                                  if (a == b) s += 4; s = s | 0; i = i + 1 }
//!           return s: the numeric operations, each comparison followed by
//!           the jump that consumes it
//!
//! codegen:
//!   generic  constants in registers: `add r`, `jump_if_less r`
//!   smi      constants as immediates: `add_smi 3`, `jump_if_less_smi n`
//!
//! engine:
//!   interp   the tail-calling interpreter (default)
//!   jit1     the copy-and-patch JIT with generic stencils only (milestone 1)
//!   jit2     with type-specialized stencils, VM registers in memory (milestone 2)
//!   jit3     with VM registers in machine registers too (milestone 3)
//!   jit4     with fused stencils too
//!   jit      with unboxed int32 in slots too
//!   tier     the default: the full JIT, starting in the interpreter and
//!            tiering up after 1000 loop back edges (on-stack replacement)
//!
//! The `jit*` engines compile eagerly, on entry, so their numbers measure the
//! JIT code alone.
//!
//! JIT timings include compilation, which happens on entry (and again after
//! a side exit; `double` exits once when `s` overflows int32).
//! ```
//!
//! With `CLOISON_JIT_DUMP=<file>`, the hot machine code the JIT compiled last
//! is written to `<file>` after the run (see `scripts/jit-asm.sh`).
//!
//! The bytecode is quickened (`cloison::bytecode::quicken`: comparison + jump
//! superinstructions) unless `CLOISON_NO_QUICKEN` is set.
//!
//! `n` is baked into the bytecode, as `i < 100000000` would be. In `double`,
//! `s` leaves the int32 range after ~65k iterations and `add` takes its slow
//! path from then on. `scripts/bench.sh` runs the whole matrix under `perf`.

use std::process::exit;
use std::time::Instant;
use cloison::bytecode::{Arg, Assembler, Op};
use cloison::jit::JitFunction;
use cloison::value::Value;

const S: u8 = 0;
const I: u8 = 1;
const LIMIT: u8 = 2;
const K: u8 = 3;
/// First of the eight `wide` accumulators.
const A1: u8 = 4;

fn usage() -> ! {
    eprintln!(
        "usage: dispatch_bench <int|const|chain|double|wide> <generic|smi> [n] [interp|jit1|jit2|jit3|jit4|jit|tier]"
    );
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
    let engine = args.get(4).map_or("interp", String::as_str);
    if !matches!(
        engine,
        "interp" | "jit1" | "jit2" | "jit3" | "jit4" | "jit" | "tier"
    ) {
        usage()
    }
    let nf = n as f64;

    let mut a = Assembler::new();
    let top = a.label();
    a.lda_smi(0).star(S).star(I);
    a.lda_smi(n).star(LIMIT);
    a.lda_smi(3).star(K).lda_smi(0);
    for r in A1..A1 + 8 {
        a.star(r);
    }
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
        "wide" => {
            // a_k in registers A1.., added an immediate k (both codegens).
            for k in 1..=8u8 {
                a.ldar(A1 + k - 1).add_smi(k as i32).star(A1 + k - 1);
            }
            8.0 * nf
        }
        "cmp" => {
            let (ra, rb) = (A1, A1 + 1);
            let (s1, s2, s3) = (a.label(), a.label(), a.label());
            a.ldar(I).emit(Op::ModSmi, &[Arg::Imm(7)]).star(ra);
            a.ldar(I).emit(Op::BitAndSmi, &[Arg::Imm(15)]).star(rb);
            a.ldar(ra).emit(Op::TestLessThan, &[Arg::Reg(rb)]);
            a.emit(Op::JumpIfFalse, &[Arg::Label(s1)]);
            a.ldar(S).inc().star(S);
            a.bind(s1);
            a.ldar(I).emit(Op::BitAndSmi, &[Arg::Imm(3)]);
            a.emit(Op::TestEqualSmi, &[Arg::Imm(0)]);
            a.emit(Op::JumpIfFalse, &[Arg::Label(s2)]);
            a.ldar(S).add_smi(2).star(S);
            a.bind(s2);
            a.ldar(rb).emit(Op::TestEqual, &[Arg::Reg(ra)]);
            a.emit(Op::JumpIfFalse, &[Arg::Label(s3)]);
            a.ldar(S).add_smi(4).star(S);
            a.bind(s3);
            a.ldar(S).emit(Op::BitOrSmi, &[Arg::Imm(0)]).star(S);
            // The body repeats with period lcm(7, 16) = 112: count one
            // period, not n iterations (perf would measure this loop too).
            let step = |i: i64| {
                let (x, y) = (i % 7, i & 15);
                (y < x) as i64 + 2 * (i & 3 == 0) as i64 + 4 * (x == y) as i64
            };
            let (period, n) = (112, n as i64);
            let full: i64 = (0..period).map(step).sum();
            ((n / period) * full + (0..n % period).map(step).sum::<i64>()) as f64
        }
        _ => usage(),
    };
    a.ldar(I).inc().star(I);
    if smi {
        a.jump_if_less_smi(n, top);
    } else {
        a.jump_if_less(LIMIT, top);
    }
    a.ldar(if program == "wide" { A1 + 7 } else { S }).ret();
    let mut code = a.finish();
    if std::env::var_os("CLOISON_NO_QUICKEN").is_none() {
        cloison::bytecode::quicken(&mut code);
    }

    let mut regs = [Value::UNDEFINED; 12];
    let mut jit = match engine {
        "jit" => Some(JitFunction::new(&code).eager()),
        "jit1" => Some(JitFunction::new(&code).eager().unspecialized()),
        "jit2" => Some(JitFunction::new(&code).eager().without_register_homes()),
        "jit3" => Some(JitFunction::new(&code).eager().without_fusion()),
        "jit4" => Some(JitFunction::new(&code).eager().without_unboxed_ints()),
        "tier" => Some(JitFunction::new(&code)),
        _ => None,
    };
    let start = Instant::now();
    let result = match &mut jit {
        Some(f) => unsafe { f.run(&mut regs) },
        None => unsafe { cloison::interp::run(&code, &mut regs) },
    };
    let elapsed = start.elapsed();
    if let (Some(f), Ok(path)) = (&jit, std::env::var("CLOISON_JIT_DUMP")) {
        let code = f.last_code().expect("compiled code");
        std::fs::write(path, code.main_code()).expect("writing CLOISON_JIT_DUMP");
    }

    assert_eq!(
        result.as_number(),
        Some(expected),
        "{program}/{codegen}: got {result:?}"
    );
    println!(
        "{program}/{codegen}/{engine} n={n} result={result:?} time={:.3}s {:.2} ns/iter{}",
        elapsed.as_secs_f64(),
        elapsed.as_nanos() as f64 / nf,
        jit.map_or(String::new(), |f| format!(" {:?}", f.stats)),
    );
}
