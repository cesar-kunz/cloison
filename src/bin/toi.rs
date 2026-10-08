//! `toi FILE`: run a script (`.js`) or a bytecode module in text form
//! (`.asm`, see `src/asm.rs`) and print its completion value.
//!
//! `toi --dump FILE.js` prints the bytecode the script compiles to instead,
//! in the text form.
//!
//! `TOI_TIME` set: also prints, to stderr, the milliseconds from compiling
//! to completion (wall clock; process startup excluded, as V8's in-process
//! timings exclude Node's: see `scripts/tiers.sh`).

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let (dump, path) = match &args[..] {
        [p] => (false, p),
        [flag, p] if flag == "--dump" => (true, p),
        _ => {
            eprintln!("usage: toi [--dump] FILE.js|FILE.asm");
            return ExitCode::from(2);
        }
    };
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("toi: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut vm = toi::vm::Vm::new();
    let start = std::time::Instant::now();
    let module = if path.ends_with(".js") {
        toi::js::compile(&src)
    } else {
        toi::asm::parse(&src)
    };
    let module = match module {
        Ok(m) => m,
        Err(e) => {
            eprintln!("toi: {path}:{e}");
            return ExitCode::FAILURE;
        }
    };
    if dump {
        print!("{}", toi::asm::disassemble(&module));
        return ExitCode::SUCCESS;
    }
    match vm.run(&module) {
        Ok(v) => {
            if std::env::var_os("TOI_TIME").is_some() {
                eprintln!("time {:.3}", start.elapsed().as_secs_f64() * 1e3);
            }
            println!("{}", toi::vm::display(v));
            // `TOI_JIT_TIME`: the JIT's lowering and compiling time.
            if std::env::var_os("TOI_JIT_TIME").is_some() {
                let ms = |a: &std::sync::atomic::AtomicU64| {
                    a.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6
                };
                eprintln!(
                    "lower {:.3} ms, compile {:.3} ms",
                    ms(&toi::jit::LOWER_NS),
                    ms(&toi::jit::COMPILE_NS)
                );
                eprintln!(
                    "lowers {} compiles {}",
                    toi::jit::LOWERS.load(std::sync::atomic::Ordering::Relaxed),
                    toi::jit::COMPILES.load(std::sync::atomic::Ordering::Relaxed)
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("toi: uncaught {e}");
            ExitCode::FAILURE
        }
    }
}
