//! The `cloison` binary, run as a process.

use std::io::Read;
use std::process::{Command, Stdio};

/// `cloison` running `src` (a script written to a file of its own).
fn cloison(test: &str, src: &str) -> Command {
    let path = std::env::temp_dir().join(format!("cloison-cli-{test}-{}.js", std::process::id()));
    std::fs::write(&path, src).unwrap();
    let mut c = Command::new(env!("CARGO_BIN_EXE_cloison"));
    c.arg(path);
    c
}

/// Standard output failing doesn't abort the process (issue #68:
/// `print`'s `println!` panicked, and `panic = "abort"`): full, `print`
/// and the completion value are dropped and it exits 0; closed by its
/// reader (`| head`), it ends of SIGPIPE, as SpiderMonkey's and
/// QuickJS's shells.
#[test]
fn stdout_failing_does_not_abort() {
    use std::os::unix::process::ExitStatusExt;
    let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let s = cloison("full", "print(1); 2").stdout(full).status().unwrap();
    assert_eq!(s.code(), Some(0), "{s}");

    let mut child = cloison("pipe", "for (var i = 0; ; i++) print(i);").stdout(Stdio::piped()).spawn().unwrap();
    let mut out = child.stdout.take().unwrap();
    let mut first = [0; 2];
    out.read_exact(&mut first).unwrap();
    assert_eq!(&first, b"0\n");
    drop(out);
    let s = child.wait().unwrap();
    assert_eq!(s.signal(), Some(libc::SIGPIPE), "{s}");
}

/// Standard error failing doesn't abort the process either (#68: an
/// uncaught exception's message and an agent's went out by `eprintln!`,
/// which panics): the message is dropped and the status stays that of
/// the exception, 1, or of the script, 0, for an agent's.
#[test]
fn stderr_failing_does_not_abort() {
    let full = || std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
    let s = cloison("err-full", "throw new Error('x')").stderr(full()).status().unwrap();
    assert_eq!(s.code(), Some(1), "{s}");

    let src = "$262.agent.start('throw new Error(1)');\n\
               while ($262.agent.getReport() == null && Date.now() < start + 500) $262.agent.sleep(10);\n\
               print('done')";
    let out = cloison("agent-err-full", &format!("var start = Date.now();\n{src}")).stderr(full()).output().unwrap();
    assert_eq!(out.status.code(), Some(0), "{}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "done\nundefined\n");
}
