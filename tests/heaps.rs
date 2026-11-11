//! Running out of heaps: a process has room for `heap::HEAPS` at once,
//! and past them making a VM fails rather than aborting. (A file of its
//! own: its own process, so other tests' VMs don't find the heaps taken.)

use cloison::vm::{Vm, display};

/// `Vm::try_new` is `None` once every heap is taken, and a heap freed is
/// taken again; agents starting agents past them get a `TypeError` in
/// the one that starts them (issue #66: `Heap::new` panicked in the
/// agent's thread, and `panic = "abort"` ended the process).
#[test]
fn heaps_run_out_without_aborting() {
    let mut vms = Vec::new();
    while let Some(vm) = Vm::try_new() {
        vms.push(vm);
    }
    assert_eq!(vms.len(), cloison::heap::HEAPS);
    vms.pop();
    assert!(Vm::try_new().is_some());
    drop(vms);

    // Eight workers start agents till the heaps run out, each once the
    // broadcast says all eight are up: before, two of them could take
    // every heap left while the coordinator started the rest (32 GiB
    // slices have room for 512, but a TSan build's 8 GiB only for 128),
    // and its uncaught `TypeError` left the main agent waiting. A start
    // failing there is reported, and every wait gives up after 10
    // minutes (TSan's tests/js.rs takes 7).
    let src = r#"
        var deadline = $262.agent.monotonicNow() + 600000;
        $262.agent.start(`
            var deadline = $262.agent.monotonicNow() + 600000;
            var seen = [];
            try {
                for (var i = 0; i < 8; i++) $262.agent.start(\`
                    $262.agent.receiveBroadcast(function () {
                        var e = "none";
                        try { for (var j = 0; j < 64; j++) $262.agent.start('$262.agent.receiveBroadcast(function () {})'); }
                        catch (x) { e = x.constructor.name + ": " + x.message; }
                        $262.agent.report(e);
                    });
                \`);
            } catch (x) {
                seen.push("starting the workers: " + x.constructor.name + ": " + x.message);
            }
            if (seen.length == 0) {
                $262.agent.broadcast(new SharedArrayBuffer(8));
                while (seen.length < 8 && $262.agent.monotonicNow() < deadline) {
                    var r = $262.agent.getReport();
                    if (r == null) $262.agent.sleep(10); else seen.push(r);
                }
                if (seen.length < 8) seen.push("timed out with " + seen.length + " reports");
                try { $262.agent.start(''); seen.push("started"); }
                catch (x) { seen.push(x.constructor.name + ": " + x.message); }
            }
            $262.agent.report(seen.filter(function (s) { return s != "none"; }).join("|"));
        `);
        var r;
        while ((r = $262.agent.getReport()) == null && $262.agent.monotonicNow() < deadline) $262.agent.sleep(10);
        r == null ? "timed out" : r
    "#;
    let mut vm = Vm::new();
    let r = display(vm.run(&cloison::js::compile(src).unwrap()).unwrap());
    let error = format!("TypeError: $262.agent.start: more than {} heaps alive at once", cloison::heap::HEAPS);
    assert!(r.split('|').all(|e| e == error), "{r}");
}
