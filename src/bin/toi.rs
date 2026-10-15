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

extern "C" fn on_segv(_: i32, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
    unsafe {
        #[cfg(target_os = "macos")]
        let addr = (*info).si_addr as usize;
        #[cfg(not(target_os = "macos"))]
        let addr = (*info).si_addr() as usize;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        let pc = (*(*(ctx as *mut libc::ucontext_t)).uc_mcontext).__ss.__pc as usize;
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        let pc = { let _ = ctx; 0usize };
        eprintln!("SIGSEGV at {addr:#x}, pc {pc:#x}");
        eprintln!("{}", std::backtrace::Backtrace::force_capture());
        libc::_exit(139);
    }
}

fn main() -> ExitCode {
    if std::env::var_os("TOI_SEGV").is_some() {
        unsafe {
            let stack = vec![0u8; 1 << 20].leak();
            let ss = libc::stack_t { ss_sp: stack.as_mut_ptr().cast(), ss_size: stack.len(), ss_flags: 0 };
            libc::sigaltstack(&ss, std::ptr::null_mut());
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_segv as *const () as usize;
            sa.sa_flags = libc::SA_SIGINFO | libc::SA_ONSTACK;
            libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut());
            libc::sigaction(libc::SIGBUS, &sa, std::ptr::null_mut());
        }
    }
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
    let result = vm.run(&module);
    let uncaught = result.as_ref().err().map(|e| vm.uncaught_message(e));
    // `TOI_GC_VERIFY`: walk the heap, checking every cell (see `heap`).
    if std::env::var_os("TOI_GC_VERIFY").is_some() {
        let cells = vm.heap.verify();
        eprintln!("heap: {cells} cells, {} MiB of blocks", vm.heap.allocated() >> 20);
    }
    // The process exits next: freeing the VM's shapes and code one by one
    // would only cost time (about 100k instructions).
    std::mem::forget(vm);
    // `TOI_EXITS`: exits from compiled code to the interpreter, and entries.
    if std::env::var_os("TOI_EXITS").is_some() {
        eprint!("{}", toi::jit::exits_report());
    }
    match result {
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
            // `TOI_SIZES`: what the run compiled, one line of `name=value`
            // (`scripts/octane-sizes.sh` reads it).
            if std::env::var_os("TOI_SIZES").is_some() {
                let n = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "sizes functions={} stubs={} bytecode={} jit_hot={} jit_ool={} jit_exits={} jit_total={} compiles={} lowers={} compile_ms={:.1} lower_ms={:.1}",
                    n(&toi::vm::FUNCTIONS),
                    n(&toi::vm::STUBS),
                    n(&toi::vm::BYTECODE_BYTES),
                    n(&toi::jit::JIT_HOT_BYTES),
                    n(&toi::jit::JIT_OOL_BYTES),
                    n(&toi::jit::JIT_EXIT_BYTES),
                    n(&toi::jit::JIT_BYTES),
                    n(&toi::jit::COMPILES),
                    n(&toi::jit::LOWERS),
                    n(&toi::jit::COMPILE_NS) as f64 / 1e6,
                    n(&toi::jit::LOWER_NS) as f64 / 1e6,
                );
            }
            // `TOI_COUNT`: hot code's stencil executions.
            if std::env::var_os("TOI_COUNT").is_some() {
                eprint!("{}", toi::jit::count_report());
            }
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("toi: uncaught {}", uncaught.unwrap());
            ExitCode::FAILURE
        }
    }
}
