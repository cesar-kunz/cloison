//! `cloison FILE`: run a script (`.js`) or a bytecode module in text form
//! (`.asm`, see `src/asm.rs`) and print its completion value.
//!
//! `cloison --dump FILE.js` prints the bytecode the script compiles to instead,
//! in the text form.
//!
//! `cloison --module FILE` (or a `.mjs` file) runs a module, and the modules
//! it imports, instead; `--prelude SCRIPT` runs a script first (test262's
//! harness, before a module test).
//!
//! `--no-block`: `Atomics.wait` throws rather than block the main agent
//! (test262's `CanBlockIsFalse`).
//!
//! `CLOISON_TIME` set: also prints, to stderr, the milliseconds from compiling
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
    if std::env::var_os("CLOISON_SEGV").is_some() {
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
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let mut prelude = None;
    let mut module = false;
    let mut no_block = false;
    while args.len() > 1 {
        match args[0].as_str() {
            "--prelude" if args.len() > 2 => {
                prelude = Some(args.remove(1));
                args.remove(0);
            }
            "--module" => {
                module = true;
                args.remove(0);
            }
            "--no-block" => {
                no_block = true;
                args.remove(0);
            }
            _ => break,
        }
    }
    let (dump, path) = match &args[..] {
        [p] => (false, p),
        [flag, p] if flag == "--dump" => (true, p),
        _ => {
            eprintln!("usage: cloison [--dump] FILE.js|FILE.asm\n       cloison [--prelude SCRIPT] [--module] [--no-block] FILE");
            return ExitCode::from(2);
        }
    };
    if module || path.ends_with(".mjs") {
        return run_module(prelude.as_deref(), path, no_block);
    }
    let src = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("cloison: {path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let mut vm = cloison::vm::Vm::new();
    vm.set_can_block(!no_block);
    vm.set_script_path(path);
    let start = std::time::Instant::now();
    let module = if path.ends_with(".js") {
        cloison::js::compile(&src)
    } else {
        cloison::asm::parse(&src)
    };
    let module = match module {
        Ok(m) => m,
        Err(e) => {
            eprintln!("cloison: {path}:{e}");
            return ExitCode::FAILURE;
        }
    };
    if dump {
        print!("{}", cloison::asm::disassemble(&module));
        return ExitCode::SUCCESS;
    }
    let result = vm.run(&module);
    let uncaught = result.as_ref().err().map(|e| vm.uncaught_message(e));
    // `CLOISON_GC_VERIFY`: walk the heap, checking every cell (see `heap`).
    if std::env::var_os("CLOISON_GC_VERIFY").is_some() {
        let cells = vm.heap.verify();
        eprintln!("heap: {cells} cells, {} MiB of blocks", vm.heap.allocated() >> 20);
    }
    let mem = vm.memory_stats();
    let jit = if std::env::var_os("CLOISON_SIZES").is_some() { cloison::jit::census(&vm) } else { Default::default() };
    // (What the heap holds at the end counts for its peak too.)
    cloison::gc::HEAP_PEAK.fetch_max(vm.heap.allocated() as u64, std::sync::atomic::Ordering::Relaxed);
    // The process exits next: freeing the VM's shapes and code one by one
    // would only cost time (about 100k instructions).
    std::mem::forget(vm);
    // `CLOISON_EXITS`: exits from compiled code to the interpreter, and entries.
    if std::env::var_os("CLOISON_EXITS").is_some() {
        eprint!("{}", cloison::jit::exits_report());
    }
    match result {
        Ok(v) => {
            if std::env::var_os("CLOISON_TIME").is_some() {
                eprintln!("time {:.3}", start.elapsed().as_secs_f64() * 1e3);
            }
            println!("{}", cloison::vm::display(v));
            // `CLOISON_JIT_TIME`: the JIT's lowering and compiling time.
            if std::env::var_os("CLOISON_JIT_TIME").is_some() {
                let ms = |a: &std::sync::atomic::AtomicU64| {
                    a.load(std::sync::atomic::Ordering::Relaxed) as f64 / 1e6
                };
                eprintln!(
                    "lower {:.3} ms, compile {:.3} ms",
                    ms(&cloison::jit::LOWER_NS),
                    ms(&cloison::jit::COMPILE_NS)
                );
                eprintln!(
                    "lowers {} compiles {}",
                    cloison::jit::LOWERS.load(std::sync::atomic::Ordering::Relaxed),
                    cloison::jit::COMPILES.load(std::sync::atomic::Ordering::Relaxed)
                );
            }
            // `CLOISON_SIZES`: what the run compiled, one line of `name=value`
            // (`scripts/octane-sizes.sh` reads it), its collections and memory,
            // others (`scripts/octane-memory.sh`), and what the JIT keeps.
            if std::env::var_os("CLOISON_SIZES").is_some() {
                let n = |a: &std::sync::atomic::AtomicU64| a.load(std::sync::atomic::Ordering::Relaxed);
                eprintln!(
                    "sizes functions={} stubs={} bytecode={} jit_hot={} jit_ool={} jit_exits={} jit_total={} compiles={} lowers={} compile_ms={:.1} lower_ms={:.1}",
                    n(&cloison::vm::FUNCTIONS),
                    n(&cloison::vm::STUBS),
                    n(&cloison::vm::BYTECODE_BYTES),
                    n(&cloison::jit::JIT_HOT_BYTES),
                    n(&cloison::jit::JIT_OOL_BYTES),
                    n(&cloison::jit::JIT_EXIT_BYTES),
                    n(&cloison::jit::JIT_BYTES),
                    n(&cloison::jit::COMPILES),
                    n(&cloison::jit::LOWERS),
                    n(&cloison::jit::COMPILE_NS) as f64 / 1e6,
                    n(&cloison::jit::LOWER_NS) as f64 / 1e6,
                );
                eprintln!(
                    "gc minors={} fulls={} pause_ms={:.1} max_pause_ms={:.2} heap_peak={} live_peak={}",
                    n(&cloison::gc::MINORS),
                    n(&cloison::gc::FULLS),
                    n(&cloison::gc::PAUSE_NS) as f64 / 1e6,
                    n(&cloison::gc::MAX_PAUSE_NS) as f64 / 1e6,
                    n(&cloison::gc::HEAP_PEAK),
                    n(&cloison::gc::LIVE_PEAK),
                );
                // (glibc's count of what malloc has handed out and not freed.)
                #[cfg(all(target_os = "linux", target_env = "gnu"))]
                let malloc = {
                    let m = unsafe { libc::mallinfo2() };
                    m.uordblks + m.hblkhd
                };
                #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
                let malloc = 0;
                let jit_bytes: usize = jit.1.iter().map(|(_, v)| v).sum();
                eprintln!(
                    "mem shapes={} shape_bytes={} codes={} code_bytes={} pinned_codes={} dead_shapes={} jit_bytes={jit_bytes} malloc={malloc}",
                    mem[0], mem[1], mem[2], mem[3], mem[4], mem[5]
                );
                // What the JIT keeps, by kind (see `jit::census`).
                let line: Vec<String> = jit.0.iter().chain(&jit.1).map(|(k, v)| format!("{k}={v}")).collect();
                eprintln!("jit {}", line.join(" "));
            }
            // `CLOISON_COUNT`: hot code's stencil executions.
            if std::env::var_os("CLOISON_COUNT").is_some() {
                eprint!("{}", cloison::jit::count_report());
            }
            ExitCode::SUCCESS
        }
        Err(_) => {
            eprintln!("cloison: uncaught {}", uncaught.unwrap());
            ExitCode::FAILURE
        }
    }
}

/// `cloison [--prelude SCRIPT] --module FILE`: SCRIPT run, then module FILE.
fn run_module(prelude: Option<&str>, path: &str, no_block: bool) -> ExitCode {
    let mut vm = cloison::vm::Vm::new();
    vm.set_can_block(!no_block);
    if let Some(p) = prelude {
        let r = std::fs::read_to_string(p).map_err(|e| e.to_string()).and_then(|src| cloison::js::compile(&src));
        let module = match r {
            Ok(m) => m,
            Err(e) => {
                eprintln!("cloison: {p}:{e}");
                return ExitCode::FAILURE;
            }
        };
        vm.set_script_path(p);
        if let Err(e) = vm.run(&module) {
            eprintln!("cloison: uncaught {}", vm.uncaught_message(&e));
            return ExitCode::FAILURE;
        }
    }
    let r = vm.run_module(path);
    let code = match r {
        Ok(()) => ExitCode::SUCCESS,
        // (Not "uncaught": test262's runners tell early errors so.)
        Err(cloison::vm::ModuleError::Early(m)) => {
            eprintln!("cloison: {m}");
            ExitCode::FAILURE
        }
        Err(cloison::vm::ModuleError::Thrown(e)) => {
            eprintln!("cloison: uncaught {}", vm.uncaught_message(&e));
            ExitCode::FAILURE
        }
    };
    std::mem::forget(vm);
    code
}
