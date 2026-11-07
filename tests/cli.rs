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
/// QuickJS's shells. (Full: Linux's `/dev/full`, which macOS hasn't.)
#[test]
fn stdout_failing_does_not_abort() {
    use std::os::unix::process::ExitStatusExt;
    if cfg!(target_os = "linux") {
        let full = std::fs::OpenOptions::new().write(true).open("/dev/full").unwrap();
        let s = cloison("full", "print(1); 2").stdout(full).status().unwrap();
        assert_eq!(s.code(), Some(0), "{s}");
    }

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
/// the exception, 1, or of the script, 0, for an agent's. (Linux's
/// `/dev/full`.)
#[test]
#[cfg(target_os = "linux")]
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

/// An agent whose thread can't be spawned gives its heap back (#66: the
/// start reserves it first): with threads refused (`RLIMIT_NPROC`), 600
/// starts, more than the process has heaps, all fail to spawn, none
/// finds the heaps gone. (Linux's `RLIMIT_NPROC`, and its `EAGAIN`.)
#[test]
#[cfg(target_os = "linux")]
fn agents_not_spawned_free_their_heaps() {
    use std::os::unix::process::CommandExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // (Root isn't held to RLIMIT_NPROC.)
    }
    let src = "var errors = new Set;\n\
               for (var i = 0; i < 600; i++) try { $262.agent.start(''); errors.add('started'); } catch (e) { errors.add(e.message); }\n\
               print([...errors].join('|'))";
    let mut c = cloison("no-threads", src);
    c.env("CLOISON_GC_STW", "1");
    unsafe {
        c.pre_exec(|| {
            let mut l = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
            libc::getrlimit(libc::RLIMIT_NPROC, &mut l);
            l.rlim_cur = 1;
            if libc::setrlimit(libc::RLIMIT_NPROC, &l) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let out = c.output().unwrap();
    assert!(out.status.success(), "{}", out.status);
    assert_eq!(String::from_utf8_lossy(&out.stdout), "$262.agent.start: Resource temporarily unavailable (os error 11)\nundefined\n");
}

/// A local time a zone skips (spring forward) takes the offset from before
/// the transition, and one it repeats (fall back) is its earlier instant
/// (UTC(t), ES2025 21.4.1.26; #105), by the constructor and by `setHours`:
/// a gap and an overlap in four zones, `TZ` set for each run. (The values
/// are SpiderMonkey's and JSC's; named zones need the system's database.)
#[test]
fn local_times_in_zone_transitions() {
    let cases = [
        ("America/New_York", "2020, 2, 8, 2, 30", "2020, 10, 1, 1, 30", "1583652600000 3 1604208600000 1"),
        ("Europe/Berlin", "2021, 2, 28, 2, 30", "2021, 9, 31, 2, 30", "1616895000000 3 1635640200000 2"),
        ("Australia/Lord_Howe", "2020, 9, 4, 2, 15", "2020, 3, 5, 1, 45", "1601739900000 2 1586011500000 1"),
        ("America/Santiago", "2020, 8, 6, 0, 30", "2020, 3, 4, 23, 30", "1599366600000 1 1586053800000 23"),
    ];
    for (zone, gap, overlap, expected) in cases {
        if !std::path::Path::new("/usr/share/zoneinfo").join(zone).exists() {
            continue;
        }
        let src = format!(
            "var g = new Date({gap}), o = new Date({overlap});\n\
             var h = new Date(o.getTime() - 864e5); h.setHours(o.getHours(), o.getMinutes() + 1440);\n\
             if (h.getTime() !== o.getTime()) throw new Error('setHours: ' + h.getTime());\n\
             print([g.getTime(), g.getHours(), o.getTime(), o.getHours()].join(' '))"
        );
        let out = cloison(&format!("tz-{}", zone.replace('/', "-")), &src).env("TZ", zone).output().unwrap();
        assert!(out.status.success(), "{zone}: {}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), format!("{expected}\nundefined\n"), "{zone}");
    }
}
