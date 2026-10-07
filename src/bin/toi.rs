//! `toi FILE`: run a script (`.js`) or a bytecode module in text form
//! (`.asm`, see `src/asm.rs`) and print its completion value.
//!
//! `toi --dump FILE.js` prints the bytecode the script compiles to instead,
//! in the text form.

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
