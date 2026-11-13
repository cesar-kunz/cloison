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
    assert_eq!(
        String::from_utf8_lossy(&out.stdout),
        "$262.agent.start: Resource temporarily unavailable (os error 11)\nundefined\n"
    );
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

/// A grow of buffer `i` by one agent, seen through its length by a second
/// (`byteLength`), which then stores `i + 1` to a flag a third (the main
/// agent) loads: the grow happens before what follows the load there,
/// whose view of the buffer has the grown length, though the grow's
/// bookkeeping may not be done yet (#155: it skipped its catch-up, the
/// grow counted only after its length was written). The grows sleep in
/// that window (`CLOISON_TEST_GROW_DELAY`) for most runs to hit it. (An
/// element past the old length: bounded by the cached length, which the
/// catch-up brings up to date, where `length` reads the block, #156.)
#[test]
fn a_grow_seen_through_a_third_agent_reaches_its_views() {
    let src = r#"
        var N = 30;
        $262.agent.start(`var bufs = []; $262.agent.receiveBroadcast(function (sab) {
            bufs.push(sab);
            if (bufs.length <= ${N}) return;
            for (var i = 0; i < ${N}; i++) bufs[i].grow(64);
            $262.agent.leaving();
        });`);
        $262.agent.start(`var bufs = []; $262.agent.receiveBroadcast(function (sab) {
            bufs.push(sab);
            if (bufs.length <= ${N}) return;
            var flag = new Int32Array(sab);
            for (var i = 0; i < ${N}; i++) {
                while (bufs[i].byteLength === 16);
                Atomics.store(flag, 0, i + 1);
            }
            $262.agent.leaving();
        });`);
        var views = [];
        for (var i = 0; i < N; i++) {
            var sab = new SharedArrayBuffer(16, { maxByteLength: 64 });
            views.push(new Int32Array(sab));
            $262.agent.broadcast(sab);
        }
        var flag = new Int32Array(new SharedArrayBuffer(4));
        $262.agent.broadcast(flag.buffer);
        var stale = 0;
        for (var i = 0; i < N; i++) {
            while (Atomics.load(flag, 0) <= i);
            if (views[i][15] === undefined) stale++;
        }
        print("stale " + stale);
    "#;
    for (delay, no_jit) in [("2000", false), ("2000", true), ("0", false)] {
        let mut c = cloison("grow-hb", src);
        c.env("CLOISON_TEST_GROW_DELAY", delay);
        if no_jit {
            c.env("CLOISON_NO_JIT", "1");
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(String::from_utf8_lossy(&out.stdout), "stale 0\nundefined\n", "delay {delay}, no JIT {no_jit}");
    }
}

/// `src` run under the JIT and by the interpreter alone, with environment
/// `env`: it prints `expected`.
fn prints_in_both_modes(test: &str, src: &str, env: &[(&str, &str)], expected: &str) {
    for no_jit in [false, true] {
        let mut c = cloison(test, src);
        c.envs(env.iter().copied());
        if no_jit {
            c.env("CLOISON_NO_JIT", "1");
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
        assert_eq!(
            String::from_utf8_lossy(&out.stdout),
            format!("{expected}\nundefined\n"),
            "{env:?}, no JIT {no_jit}"
        );
    }
}

/// A view's constructor reads its buffer's length seq-cst: once it has
/// seen another agent's grow of `sab2`, that agent's grow of `sab1` before
/// reaches `sab1`'s views too, an element past the old length included
/// (#219: the constructors read the block without a catch-up).
#[test]
fn a_view_constructor_seeing_a_grow_catches_up() {
    for read in ["new Int8Array(sab2).length", "new DataView(sab2).byteLength", "new Int8Array(sab2, 0).length"] {
        let src = format!(
            "$262.agent.start(`
               var first, calls = 0;
               $262.agent.receiveBroadcast(function (sab) {{
                 if (++calls === 1) {{ first = sab; return; }}
                 $262.agent.sleep(50);
                 first.grow(64); sab.grow(64); $262.agent.leaving();
               }});`);
             var sab1 = new SharedArrayBuffer(16, {{ maxByteLength: 64 }});
             var sab2 = new SharedArrayBuffer(16, {{ maxByteLength: 64 }});
             var ta1 = new Int32Array(sab1);
             $262.agent.broadcast(sab1, 1);
             $262.agent.broadcast(sab2, 2);
             var t0 = $262.agent.monotonicNow(), seen = 0;
             while ((seen = {read}) !== 64 && $262.agent.monotonicNow() - t0 < 5000) ;
             var e = ta1[15];
             print(seen, e === 0 ? 'OK' : 'STALE', ta1.length);"
        );
        prints_in_both_modes("ctor-catch-up", &src, &[], "64 OK 16");
    }
}

/// An agent sent one buffer twice has two buffer objects on its block: a
/// view made on one after another agent's grow, the other's views see the
/// grow too (#219).
#[test]
fn a_view_constructor_catches_up_a_blocks_aliases() {
    let src = r#"
        $262.agent.start(`
          var s = [];
          $262.agent.receiveBroadcast(function (sab) {
            s.push(sab);
            if (s.length < 2) return;
            var tb = new Int8Array(s[1]);
            $262.agent.report("ready");
            var t0 = $262.agent.monotonicNow(), n = 0;
            while ((n = new Int8Array(s[0]).length) !== 64 && $262.agent.monotonicNow() - t0 < 5000) ;
            var e = tb[63];
            $262.agent.report(n + " " + (e === 0 ? "OK" : "INCOHERENT") + " " + tb.length);
            $262.agent.leaving();
          });`);
        var sab = new SharedArrayBuffer(16, { maxByteLength: 64 });
        $262.agent.broadcast(sab); $262.agent.broadcast(sab);
        function report() {
            var r, t0 = Date.now();
            while ((r = $262.agent.getReport()) === null) if (Date.now() - t0 > 10000) throw new Error("no report");
            return r;
        }
        report();
        sab.grow(64);
        print(report());
    "#;
    prints_in_both_modes("ctor-aliases", src, &[], "64 OK 64");
}

/// A loop polling a growable shared buffer's view's length (or another
/// read the spec makes seq-cst: the getters, 23.2.3 and 25.3.4, the array
/// iterator's, ValidateTypedArray's, OwnPropertyKeys') sees another
/// agent's grow (#156: the cached length, never read again), at the top
/// level and in a function compiled on a plain typed array first. (The
/// grow waits for `broadcast`, which catches up, to have returned.)
/// Fixed-length views keep their lengths.
#[test]
fn polling_a_shared_views_length_sees_a_grow() {
    let reads = [
        "ia.length === 4",
        "len(ia) === 4",
        "ia.byteLength === 16",
        "dv.byteLength === 16",
        "[...ia].length === 4",
        "ia.slice().length === 4",
        "Object.keys(ia).length === 4",
        "u8.toHex().length === 32",
        "u8.toBase64().length === 24",
        "u8.setFromHex('00'.repeat(64)).written === 16",
        "u8.setFromBase64('A'.repeat(88)).written === 15",
    ];
    for read in reads {
        let src = format!(
            "function len(ta) {{ return ta.length; }}
             var plain = new Int32Array(4);
             for (var i = 0; i < 20000; i++) len(plain);
             $262.agent.start(`$262.agent.receiveBroadcast(function (sab) {{ $262.agent.sleep(20); sab.grow(64); $262.agent.leaving(); }});`);
             var sab = new SharedArrayBuffer(16, {{ maxByteLength: 64 }});
             var ia = new Int32Array(sab), dv = new DataView(sab), u8 = new Uint8Array(sab);
             var fixed = new Int32Array(sab, 4, 2), fixed_dv = new DataView(sab, 0, 8);
             $262.agent.broadcast(sab);
             var t0 = Date.now();
             while ({read} && Date.now() - t0 < 5000) ;
             print(ia.length, dv.byteLength, u8.length, fixed.length, fixed.byteOffset, fixed_dv.byteLength);"
        );
        prints_in_both_modes("poll-length", &src, &[], "16 64 64 2 4 8");
    }
}

/// #155's and #219's sequences under stress, three agents: one grows each
/// buffer in turn; a second sees each grow through its view's `length`,
/// checks that it and the grows before reached its views (an element past
/// the old length, bounded by the cached length), and stores `i + 1` to a
/// flag; the main agent, once it loads that, checks its view. The grows
/// sleep in their windows (`CLOISON_TEST_GROW_DELAY`).
#[test]
fn three_agents_see_grows_coherently() {
    let src = r#"
        var N = 30;
        $262.agent.start(`var bufs = []; $262.agent.receiveBroadcast(function (sab) {
            bufs.push(sab);
            if (bufs.length <= ${N}) return;
            for (var i = 0; i < ${N}; i++) bufs[i].grow(64);
            $262.agent.leaving();
        });`);
        $262.agent.start(`var views = []; $262.agent.receiveBroadcast(function (sab) {
            views.push(new Int32Array(sab));
            if (views.length <= ${N}) return;
            var flag = views[${N}], bad = 0, t0 = Date.now();
            for (var i = 0; i < ${N}; i++) {
                while (views[i].length === 4 && Date.now() - t0 < 5000);
                for (var j = 0; j <= i; j++) if (views[j][15] === undefined) bad++;
                Atomics.store(flag, 0, i + 1);
            }
            Atomics.store(flag, 1, bad + 1);
            $262.agent.leaving();
        });`);
        var views = [];
        for (var i = 0; i < N; i++) {
            var sab = new SharedArrayBuffer(16, { maxByteLength: 64 });
            views.push(new Int32Array(sab));
            $262.agent.broadcast(sab);
        }
        var flag = new Int32Array(new SharedArrayBuffer(8));
        $262.agent.broadcast(flag.buffer);
        // (Bounded: an agent that died fails the test, not hangs it.)
        var t0 = Date.now();
        function wait(until) {
            while (!until()) if (Date.now() - t0 > 10000) throw new Error("timed out at flag " + Atomics.load(flag, 0));
        }
        var stale = 0;
        for (var i = 0; i < N; i++) {
            wait(function () { return Atomics.load(flag, 0) > i; });
            if (views[i][15] === undefined) stale++;
        }
        wait(function () { return Atomics.load(flag, 1) !== 0; });
        print("stale " + stale + ", poller " + (Atomics.load(flag, 1) - 1));
    "#;
    for delay in ["2000", "0"] {
        prints_in_both_modes("grow-three", src, &[("CLOISON_TEST_GROW_DELAY", delay)], "stale 0, poller 0");
    }
}

/// Collecting at every safepoint (`CLOISON_GC_STRESS`) promotes what is
/// live at each, and a loop of async iteration left the old space to grow
/// with that garbage until `FULL_MIN`, every minor collection scanning
/// its cards: time grew with the square of the loop's length (#127; 3
/// MiB of blocks at 2,500 rounds). Full collections now keep it small.
#[test]
fn stress_collections_keep_an_async_loop_small() {
    let src = "var n = 0;
        async function* ag() { yield 1; }
        async function main() { for (var k = 0; k < 2500; k++) { for await (var z of ag()) n++; } print(n); }
        main();";
    for no_jit in [false, true] {
        let mut c = cloison("stress-async", src);
        c.env("CLOISON_GC_STRESS", "1").env("CLOISON_GC_STATS", "1");
        if no_jit {
            c.env("CLOISON_NO_JIT", "1");
        }
        let out = c.output().unwrap();
        assert!(out.status.success(), "{}", out.status);
        assert!(String::from_utf8_lossy(&out.stdout).starts_with("2500\n"));
        let most = String::from_utf8_lossy(&out.stderr)
            .lines()
            .filter_map(|l| l.split_once(" MiB of blocks")?.0.rsplit(' ').next()?.parse::<usize>().ok())
            .max();
        assert!(most.is_some_and(|m| m <= 1), "{most:?} MiB of blocks, no JIT {no_jit}");
    }
}

/// The output of `cloison` running `src` (JIT off if `no_jit`), killed if
/// it hasn't ended within 10 seconds (then `None`).
fn output_within(test: &str, src: &str, no_jit: bool) -> Option<std::process::Output> {
    let mut c = cloison(test, src);
    if no_jit {
        c.env("CLOISON_NO_JIT", "1");
    }
    let mut child = c.stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while child.try_wait().unwrap().is_none() {
        if std::time::Instant::now() > deadline {
            child.kill().unwrap();
            child.wait().unwrap();
            return None;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    Some(child.wait_with_output().unwrap())
}

/// An async waiter no agent can notify doesn't keep the process alive
/// (#152, as SpiderMonkey's and V8's shells): one on a buffer no other
/// agent has, or one an agent had and left, the event loop stops waiting
/// for; one an agent notifies, it waits for, and one with a time too.
#[test]
fn a_waiter_no_one_can_notify_ends_the_event_loop() {
    let cases = [
        ("Atomics.waitAsync(new Int32Array(new SharedArrayBuffer(64)), 0, 0); print(1)", "1"),
        (
            "var ia = new Int32Array(new SharedArrayBuffer(64));
             $262.agent.start('$262.agent.receiveBroadcast(function (sab) { $262.agent.sleep(200); $262.agent.leaving(); });');
             $262.agent.broadcast(ia.buffer);
             Atomics.waitAsync(ia, 0, 0).value.then(function (v) { print('left', v); });
             print('end')",
            "end",
        ),
        (
            "var ia = new Int32Array(new SharedArrayBuffer(64));
             $262.agent.start('$262.agent.receiveBroadcast(function (sab) { $262.agent.sleep(200); Atomics.notify(new Int32Array(sab), 0); $262.agent.leaving(); });');
             $262.agent.broadcast(ia.buffer);
             Atomics.waitAsync(ia, 0, 0).value.then(function (v) { print('notified', v); });
             Atomics.waitAsync(ia, 4, 0, 100).value.then(function (v) { print('timed', v); });
             Atomics.waitAsync(new Int32Array(new SharedArrayBuffer(8)), 0, 0).value.then(function (v) { print('alone', v); });
             void 0",
            "timed timed-out\nnotified ok",
        ),
    ];
    for (i, (src, expected)) in cases.into_iter().enumerate() {
        for no_jit in [false, true] {
            let out = output_within(&format!("wait-async-{i}"), src, no_jit).expect("didn't end");
            assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
            let want = format!("{expected}\nundefined\n");
            assert_eq!(String::from_utf8_lossy(&out.stdout), want, "case {i}, no JIT {no_jit}");
        }
    }
}
