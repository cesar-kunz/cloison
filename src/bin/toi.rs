//! `toi FILE`: run a bytecode module in text form (`.asm`, see `src/asm.rs`)
//! and print its completion value. There is no JS front end yet.

use std::process::ExitCode;

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let [path] = &args[..] else {
        eprintln!("usage: toi FILE.asm");
        return ExitCode::from(2);
    };
    if path.ends_with(".js") {
        eprintln!("toi: no JS front end yet; give a bytecode listing (.asm)");
        return ExitCode::from(2);
    }
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("toi: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let module = match toi::asm::parse(&src) {
        Ok(m) => m,
        Err(e) => {
            eprintln!("toi: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut vm = toi::vm::Vm::new();
    match vm.run(&module) {
        Ok(v) => {
            println!("{}", toi::vm::display(v));
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("toi: uncaught {e}");
            ExitCode::FAILURE
        }
    }
}
