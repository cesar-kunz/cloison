//! The JS front end: it parses every microbenchmark, compiles the ones the
//! instruction set covers to exactly the reference listings in
//! `bench/bytecode/`, and the compiled scripts compute what they should, in
//! the interpreter and through the JIT.

use std::path::{Path, PathBuf};

use cloison::vm::{Vm, display};

fn files(dir: &str, ext: &str) -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join(dir);
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == ext))
        .collect();
    v.sort();
    v
}

/// Run `src` with the interpreter only, tiering up after 1 and 2 back
/// edges, and with the default; all must agree.
fn run(src: &str) -> Result<String, String> {
    let module = cloison::js::compile(src)?;
    let mut result = None;
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        vm.verify_heap();
        match &result {
            None => result = Some(r),
            Some(first) => assert_eq!(first, &r, "tier at {budget}:\n{src}"),
        }
    }
    result.unwrap()
}

fn value(src: &str) -> String {
    run(src).unwrap_or_else(|e| panic!("{e}\n{src}"))
}

/// `f` on a thread with a 64 MiB stack: for an expression nested deeper
/// than the front end can go on a test thread's in debug builds.
fn big_stack<T: Send>(f: impl FnOnce() -> T + Send) -> T {
    std::thread::scope(|s| {
        let t = std::thread::Builder::new().stack_size(64 << 20).spawn_scoped(s, f).unwrap();
        t.join().unwrap_or_else(|e| std::panic::resume_unwind(e))
    })
}

/// [`value`] of a script whose long strings are cons strings of shared
/// pieces: the heap stays under 256 MB at its end (none was flattened).
fn light_value(src: &str) -> String {
    let module = cloison::js::compile(src).unwrap_or_else(|e| panic!("{e}\n{src}"));
    let mut result = None;
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).unwrap_or_else(|e| panic!("{e}\n{src}"));
        vm.verify_heap();
        let used = vm.heap_allocated();
        assert!(used < 256 << 20, "{used} bytes of heap at the end, tier at {budget}:\n{src}");
        match &result {
            None => result = Some(r),
            Some(first) => assert_eq!(first, &r, "tier at {budget}:\n{src}"),
        }
    }
    result.unwrap()
}

fn compile_error(src: &str) -> String {
    cloison::js::compile(src).expect_err(src)
}

#[test]
fn every_microbenchmark_parses() {
    for f in files("bench/micro", "js") {
        let src = std::fs::read_to_string(&f).unwrap();
        if let Err(e) = cloison::js::parse(&src) {
            panic!("{}:{}", f.display(), e.display(&src));
        }
    }
}

#[test]
fn listings_are_what_the_front_end_emits() {
    for f in files("bench/bytecode", "asm") {
        let js =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("bench/micro").join(f.with_extension("js").file_name().unwrap());
        let want = cloison::asm::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
        let got = cloison::js::compile(&std::fs::read_to_string(&js).unwrap())
            .unwrap_or_else(|e| panic!("{}:{e}", js.display()));
        let (want_text, got_text) = (cloison::asm::disassemble(&want), cloison::asm::disassemble(&got));
        assert_eq!(want_text, got_text, "{} (left: listing)", js.display());
        assert_eq!(want.functions.len(), got.functions.len());
        for (w, g) in want.functions.iter().zip(&got.functions) {
            assert_eq!(
                (&w.name, &w.code, &w.ics, &w.closures, w.nparams, w.nregs),
                (&g.name, &g.code, &g.ics, &g.closures, g.nparams, g.nregs),
                "{}",
                js.display()
            );
        }
    }
}

/// Benchmarks that compile, and run on what the VM implements so far.
const RUNNING: &[&str] = &[
    "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "17", "18", "19",
    "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31", "32", "33", "34", "35", "36", "37", "38",
    "39", "40", "41", "42", "43", "44", "45", "46", "50", "51", "52", "53", "54", "60", "61", "62", "63", "64", "65",
    "66", "67", "68", "69", "70", "71", "80", "81", "82", "83", "84", "85", "86", "87", "90", "91", "92", "93", "94",
    "100", "101", "102", "103", "104", "105", "106", "107", "108", "109", "110", "111", "112", "113", "114", "115",
    "116", "117", "118", "119", "120", "121", "122", "123", "124", "125", "126", "127", "128", "129", "130", "131",
    "132", "133", "134", "135",
];

/// A benchmark's number: its name up to the first `-`.
fn number(name: &str) -> &str {
    name.split('-').next().unwrap()
}

#[test]
fn compiled_microbenchmarks_compute_their_expected_results() {
    let mut ran = Vec::new();
    for f in files("bench/micro", "js") {
        let src = std::fs::read_to_string(&f).unwrap();
        let name = f.file_name().unwrap().to_str().unwrap();
        let expect = src.lines().find_map(|l| l.strip_prefix("// expect: ")).unwrap().trim();
        let Ok(module) = cloison::js::compile(&src) else {
            continue;
        };
        let mut vm = Vm::new();
        // Whatever runs to completion must be right; what doesn't must
        // fail on something the VM doesn't have yet.
        match vm.run(&module) {
            Ok(v) => {
                assert_eq!(display(v), expect, "{name}");
                ran.push(number(name).to_string());
            }
            Err(e) => {
                let e = e.to_string();
                assert!(e.contains("is not a function") || e.contains("is not defined"), "{name}: {e}");
            }
        }
    }
    // (Files sort as text: `100-` before `11-`.)
    ran.sort_by_key(|n| n.parse::<u32>().unwrap());
    assert_eq!(ran, RUNNING);
    // The ones that run do so through the JIT too.
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if RUNNING.contains(&number(name)) {
            let src = std::fs::read_to_string(&f).unwrap();
            let expect = src.lines().find_map(|l| l.strip_prefix("// expect: ")).unwrap();
            assert_eq!(value(&src), expect.trim(), "{name}");
        }
    }
}

/// The microbenchmarks compiled after 1 and 3 back edges: code entered in
/// the middle of loops, and exits there before types and caches settle,
/// each leaving the interpreter what it reads (registers sharing slots
/// write back only those; see `jit::assign_homes`).
#[test]
fn microbenchmarks_tiering_up_early_compute_their_expected_results() {
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if !RUNNING.contains(&number(name)) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        let expect = src.lines().find_map(|l| l.strip_prefix("// expect: ")).unwrap().trim();
        let module = cloison::js::compile(&src).unwrap();
        for budget in [1, 3] {
            let mut vm = Vm::new();
            vm.set_tier_back_edges(budget);
            let v = vm.run(&module).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(display(v), expect, "{name} (tier at {budget})");
        }
    }
}

/// The microbenchmarks with the warm tier on (`jit::Tier`): functions
/// compiled warm, tiering up to hot code at loop headers and starts, the
/// run continuing there; after 1 and 1000 back edges in the interpreter.
#[test]
fn microbenchmarks_through_the_warm_tier_compute_their_expected_results() {
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if !RUNNING.contains(&number(name)) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        let expect = src.lines().find_map(|l| l.strip_prefix("// expect: ")).unwrap().trim();
        let module = cloison::js::compile(&src).unwrap();
        for budget in [1, 1000] {
            let mut vm = Vm::new();
            vm.set_warm_tier(true);
            vm.set_tier_back_edges(budget);
            let v = vm.run(&module).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(display(v), expect, "{name} (warm, tier at {budget})");
        }
    }
}

/// `for await` over a sync iterable steps as AsyncFromSyncIterator does:
/// two jobs a value (the done step too), a rejected value seen as late.
#[test]
fn for_await_over_sync_iterables_takes_the_specs_jobs() {
    let src = r#"
var log = [];
var th = { then(r) { r("T"); } };
(async () => { for await (const x of ["s1", th]) log.push(x); log.push("end"); })();
(async () => { try { for await (const x of [Promise.reject("bad")]) log.push(x); } catch (e) { log.push("caught"); } })();
var p = Promise.resolve(); for (var i = 0; i < 8; i++) { let k = i; p = p.then(() => log.push(k)); }
p.then(() => { globalThis.out = log.join(" "); });
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("out");
    let g = vm.global_object();
    assert_eq!(display(vm.get(g, a).unwrap()), "0 s1 caught 1 2 3 T 4 5 end 6 7");
}

/// `globalThis.out` once `src` and its jobs have run.
fn out_after_jobs(src: &str) -> String {
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("out");
    let g = vm.global_object();
    display(vm.get(g, a).unwrap())
}

/// A `for await` left early awaits what `return()` gives, which must be
/// an object, and left by an exception awaits it too (however it
/// settles), before the exception goes on (AsyncIteratorClose; #110).
#[test]
fn for_await_close_awaits_return_and_checks_its_result() {
    let src = r#"
var log = [];
var p = Promise.resolve(); for (let i = 0; i < 6; i++) p = p.then(() => log.push("t" + i));
var A = (ret) => ({ [Symbol.asyncIterator]() { return { next() { return { value: 1, done: false } }, return: ret } } });
(async () => { try { for await (var x of A(() => 1)) break; log.push("a ok"); } catch (e) { log.push("a " + e.name); } })();
(async () => { try { for await (var x of A(() => ({}))) throw "boom"; } catch (e) { log.push("b " + e); } })();
(async () => { try { for await (var x of A(() => Promise.reject(3))) throw 4; } catch (e) { log.push("c " + e); } })();
(async () => { try { for await (var x of A(() => { throw 5; })) throw 4; } catch (e) { log.push("d " + e); } })();
(async () => { try { for await (var x of A(() => Promise.resolve(2))) break; } catch (e) { log.push("e " + e.name); } })();
(async () => { for await (var x of A(undefined)) break; log.push("f ok"); })();
(async () => { try { for await (var x of [1]) break; log.push("g ok"); } catch (e) { log.push("g " + e.name); } })();
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(out_after_jobs(src), "t0, d 4, f ok, t1, a TypeError, b boom, c 4, e TypeError, t2, g ok, t3, t4, t5");
}

/// What a `for await` step throws (`next()` throwing, rejecting or giving
/// a non-object) doesn't close the iterator (#111).
#[test]
fn for_await_step_errors_leave_the_iterator_open() {
    let src = r#"
var log = [];
for (const next of [() => Promise.reject(1), () => { throw 2; }, () => 3])
  (async () => {
    try {
      for await (var x of { [Symbol.asyncIterator]() { return { next, return() { log.push("closed"); return {}; } }; } });
    } catch (e) { log.push(typeof e == "object" ? e.name : e); }
  })();
var q = Promise.resolve(); for (let i = 0; i < 6; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(" "); });
"#;
    assert_eq!(out_after_jobs(src), "2 1 TypeError");
}

/// A sync iterator used asynchronously (`yield*`, `for await`,
/// `Array.fromAsync`) is closed in the job a rejected value's promise
/// rejects in, unless done (AsyncFromSyncIteratorContinuation's
/// `closeOnRejection`; #112).
#[test]
fn async_from_sync_closes_the_iterator_on_rejection() {
    let src = r#"
var log = [];
var p = Promise.resolve(); for (let i = 0; i < 6; i++) p = p.then(() => log.push("t" + i));
function S(k, done) { return { [Symbol.iterator]() { return this },
  next() { return { value: Promise.reject(k), done } },
  return() { log.push("closed " + k); return {}; } }; }
async function* f() { yield* S("y", false); }
f().next().catch(e => log.push("rejected " + e));
(async () => { try { for await (var x of S("f", false)); } catch (e) { log.push("caught " + e); } })();
Array.fromAsync(S("a", false)).catch(e => log.push("fromAsync " + e));
(async () => { try { for await (var x of S("d", true)); } catch (e) { log.push("done " + e); } })();
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        "t0, closed y, closed f, closed a, t1, caught f, done d, t2, rejected y, fromAsync a, t3, t4, t5"
    );
}

/// An async generator's `return(v)` awaits `v`: done or not started,
/// the result's value is what it settles to (a rejection rejects); at a
/// `yield`, a rejection is thrown there, and through `yield*` it's a
/// `throw()` (#102).
#[test]
fn async_generator_return_awaits_its_value() {
    let src = r#"
var log = [];
async function* h() {}
h().return(Promise.resolve("Y")).then(r => log.push("a " + r.value));
h().return(Promise.reject("Z")).then(() => log.push("b fulfilled"), e => log.push("b " + e));
h().return({ get then() { throw "T"; } }).then(() => log.push("c fulfilled"), e => log.push("c " + e));
async function* g() { try { yield 1; } catch (e) { log.push("d caught " + e); yield 2; } }
var it = g(); it.next().then(() => it.return(Promise.reject("W"))).then(r => log.push("d " + JSON.stringify(r)));
var inner = { [Symbol.asyncIterator]() { return this; },
  next() { return { value: 1, done: false }; },
  throw(e) { return { value: "thrown " + e, done: true }; },
  return(v) { return { value: "returned " + v, done: true }; } };
async function* k() { return yield* inner; }
var i1 = k(); i1.next().then(() => i1.return(Promise.resolve("R"))).then(r => log.push("e " + r.value));
var i2 = k(); i2.next().then(() => i2.return(Promise.reject("X"))).then(r => log.push("f " + r.value));
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        "a Y, b Z, c T, d caught W, d {\"value\":2,\"done\":false}, e returned R, f thrown X"
    );
}

/// An async generator's `yield*` awaits an async iterator's `next`,
/// `throw` or `return` result before checking it is an object: a
/// non-object is a TypeError jobs later, a promise of one too (#153).
#[test]
fn async_yield_star_awaits_results_before_checking_them() {
    let src = r#"
var log = [];
var p = Promise.resolve(); for (let i = 0; i < 6; i++) p = p.then(() => log.push("t" + i));
var inner = { [Symbol.asyncIterator]() { return { next() { return { value: 1, done: false } }, return() { return 5 } } } };
async function* g() { yield* inner; }
var it = g();
it.next().then(() => { it.return().then(() => log.push("a ok"), e => log.push("a " + e.name)); });
function A(res) { return { [Symbol.asyncIterator]() { return { next() { return res; } }; } }; }
async function* k(i) { yield* i; }
k(A(Promise.resolve(4))).next().then(() => log.push("b ok"), e => log.push("b " + e.name));
k(A({ then(r) { r({ value: 8, done: false }); } })).next().then(r => log.push("c " + r.value), e => log.push("c " + e.name));
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(out_after_jobs(src), "t0, t1, b TypeError, t2, c 8, t3, t4, a TypeError, t5");
}

/// `yield*`'s `throw()` to an iterator with no `throw` closes it as
/// IteratorClose does, what its `return()` throws (a rejection, async)
/// replacing the TypeError; an async generator awaits that `return()`'s
/// result first (AsyncIteratorClose) (#154).
#[test]
fn yield_star_throw_without_method_closes_with_errors() {
    let src = r#"
var log = [];
function show(e) { return typeof e == "string" ? e : e.name; }
function* g(ret) { yield* { [Symbol.iterator]() { return { next() { return { done: false }; }, return: ret }; } }; }
for (const [name, ret] of [["throws", function () { throw "rt"; }], ["prim", () => 1], ["plain", () => ({})], ["none", undefined]]) {
  var it = g(ret); it.next();
  try { it.throw(1); } catch (e) { log.push(name + " " + show(e)); }
}
async function* ag(ret) { yield* { [Symbol.asyncIterator]() { return { next() { return { done: false }; }, return: ret }; } }; }
function run(name, ret, how) {
  var it = ag(ret);
  it.next().then(() => how(it)).then(() => log.push(name + " ok"), e => log.push(name + " " + show(e)));
}
var thr = it => it.throw(1);
run("throws", function () { throw "rt"; }, thr);
run("rejects", () => Promise.reject("rj"), thr);
run("plain", () => { log.push("closing"); return {}; }, thr);
run("prim", () => 1, thr);
run("none", undefined, thr);
run("noncallable", 5, thr);
run("thenable", () => ({ then(r) { log.push("then"); r({}); } }), thr);
run("returned", () => Promise.reject("rr"), it => it.return(Promise.reject("x")));
var t = 0; (function tick() { log.push("t" + ++t); if (t < 7) Promise.resolve().then(tick); })();
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        "throws rt, prim TypeError, plain TypeError, none TypeError, t1, t2, closing, t3, then, t4, t5, \
         throws rt, rejects rj, plain TypeError, prim TypeError, none TypeError, noncallable TypeError, \
         t6, thenable TypeError, returned rr, t7"
    );
}

/// An async `yield*`'s `return(v)` to an async iterator without
/// `return()` awaits `v` itself (not an iterator result of it) and
/// returns it (test262's yield-star-return-then-getter-ticks); a
/// rejected `v` is thrown into the iterator, which has no `throw()`.
#[test]
fn async_yield_star_return_without_return_awaits_the_value() {
    let src = r#"
var log = [];
var inner = { [Symbol.asyncIterator]() { return this; }, next() { return { done: false, value: "n" }; },
  get return() { log.push("get return"); } };
async function* f() { log.push("start"); try { yield* inner; } catch (e) { log.push("caught " + e.name); } }
var p = Promise.resolve(); for (let i = 1; i < 4; i++) p = p.then(() => log.push("tick " + i));
var it = f(); it.next();
it.return({ get then() { log.push("get then"); } }).then(r => log.push("done " + r.done));
var g = f(); g.next().then(() => g.return(Promise.reject("x"))).then(r => log.push("after " + r.done));
var h = f(); h.next().then(() => h.return(Promise.resolve(7))).then(r => log.push("value " + r.value));
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        "start, start, start, tick 1, get then, tick 2, get return, get then, tick 3, get return, caught TypeError, get return, done true, after true, value 7"
    );
}

/// What a sync iterator's step throws in `for await` or `yield*` (its
/// `next()`, a non-object result, a `done` or `value` getter) rejects
/// the AsyncFromSyncIterator step's promise, awaited: seen a job later,
/// the iterator not closed (IfAbruptRejectPromise).
#[test]
fn async_from_sync_step_errors_are_awaited_rejections() {
    let src = r#"
var log = [];
var p = Promise.resolve(); for (let i = 0; i < 3; i++) p = p.then(() => log.push("t" + i));
function S(tag, kind) { return { [Symbol.iterator]() { return this; },
  next() { if (kind == "next") throw tag; if (kind == "obj") return 5;
    return { get done() { if (kind == "done") throw tag; return false; }, get value() { if (kind == "value") throw tag; return 1; } }; },
  return() { log.push("closed " + tag); return {}; } }; }
for (const k of ["next", "obj", "done", "value"]) {
  (async () => { try { for await (var x of S("f" + k, k)); } catch (e) { log.push(typeof e == "string" ? e : "f" + e.name); } })();
  async function* g() { try { yield* S("y" + k, k); } catch (e) { log.push(typeof e == "string" ? e : "y" + e.name); } }
  g().next();
  log.push(k);
}
var q = Promise.resolve(); for (let i = 0; i < 6; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        "next, obj, done, value, t0, fnext, ynext, fTypeError, yTypeError, fdone, ydone, fvalue, yvalue, t1, t2"
    );
}

/// AsyncFromSyncIterator resolves each step's promise with an iterator
/// result (ES2025 27.1.6.2.2 step 6.b, 27.1.6.4 steps 6-9), whose `then`
/// an `Object.prototype.then` getter sees (not the bare value), a
/// throwing one rejecting the step, a callable one adopting it: `return`
/// without a `return()` too.
#[test]
fn async_from_sync_resolves_by_iterator_results() {
    let src = r#"
var log = [];
function patch(f) { Object.defineProperty(Object.prototype, "then", { get: f, configurable: true }); }
var noRet = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; } }; } };
async function* g() { yield* noRet; }
var it = g();
async function main() {
  await it.next();
  patch(function () { log.push("ret " + JSON.stringify(this)); });
  var r = await it.return(3);
  delete Object.prototype.then;
  log.push("result " + JSON.stringify(r));
  patch(function () { log.push("for " + JSON.stringify(this)); });
  for await (var x of [{ k: 1 }]) log.push("got " + JSON.stringify(x));
  patch(function () { if (this.done === false) throw "boom"; });
  try { for await (var y of [5]) log.push("no"); } catch (e) { log.push("caught " + e); }
  delete Object.prototype.then;
  Object.prototype.then = function (res) { delete Object.prototype.then; log.push("thenable"); res(this); };
  for await (var z of [7]) log.push("got " + z);
}
main().then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        r#"ret {"value":3,"done":true}, ret {"value":3,"done":true}, result {"value":3,"done":true}, for {"k":1}, for {"value":{"k":1},"done":false}, got {"k":1}, for {"done":true}, caught boom, thenable, got 7"#
    );
}

/// With a callable `Object.prototype.then`, AsyncFromSyncIterator's
/// iterator result is adopted as a thenable (ES2025 27.1.6.4, the
/// promise resolve functions of 27.2.1.3.2), what that gives read as an
/// iterator result by `for await` (ForIn/OfBodyEvaluation, 14.7.5.7),
/// `yield*` (15.5.5) and `Array.fromAsync`: its `done`, not the sync
/// iterator's, in the job it settles in (V8, SpiderMonkey and JSC
/// agree).
#[test]
fn async_from_sync_thenable_results_are_read() {
    let src = r#"
var log = [];
// `Object.prototype.then`, callable for the next `n` iterator results it
// is looked up on: each handed on as `make` remakes it.
function thenable(n, make) {
  Object.defineProperty(Object.prototype, "then", { configurable: true, get() {
    if (!("done" in this) || this.made || n-- <= 0) return undefined;
    var r = this;
    return function (res) { log.push("then " + JSON.stringify(r)); res(make(r)); };
  } });
}
function ticks(tag) { var p = Promise.resolve(); for (let i = 0; i < 4; i++) p = p.then(() => log.push(tag + i)); }
var stop = r => ({ made: true, get done() { log.push("done"); return r.value == 2; }, get value() { log.push("value"); return r.value * 10; } });
async function main() {
  thenable(2, stop); ticks("a");
  for await (var x of [1, 2, 3]) log.push("for " + x);
  thenable(2, stop); ticks("b");
  async function* g() { log.push("yield* gave " + (yield* [1, 2, 3])); }
  var it = g();
  log.push("gen " + JSON.stringify(await it.next()));
  log.push("gen " + JSON.stringify(await it.next()));
  thenable(2, stop); ticks("c");
  log.push("fromAsync " + (await Array.fromAsync([1, 2, 3])));
  thenable(1, r => 5);
  try { for await (var z of [1]) log.push("no"); } catch (e) { log.push("for " + e.constructor.name); }
  var closing = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; }, return() { return { done: true }; } }; } };
  thenable(2, r => r.done ? 7 : { made: true, value: r.value });
  try { for await (var w of closing) { log.push("body " + w); break; } } catch (e) { log.push("break " + e.constructor.name); }
  delete Object.prototype.then;
}
main().then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        r#"a0, a1, then {"value":1,"done":false}, a2, done, value, for 10, a3, then {"value":2,"done":false}, done, b0, b1, then {"value":1,"done":false}, b2, done, value, b3, then {"value":10,"done":false}, done, value, gen {"made":true,"done":false,"value":100}, gen {"value":2,"done":false}, c0, c1, then {"value":1,"done":false}, c2, done, value, c3, then {"value":2,"done":false}, done, fromAsync 10, then {"value":1,"done":false}, for TypeError, then {"value":1,"done":false}, body 1, then {"done":true}, break TypeError"#
    );
}

/// With a `then` on `Object.prototype` that isn't callable, the step's
/// promise is fulfilled with the iterator result it was looked up on
/// (ES2025 27.1.6.4, 27.2.1.3.2): what its getter changes on it, then or
/// later (before the consumer reads it, a job after), is what `for
/// await`, `yield*`, `Array.fromAsync` and `return` without a `return()`
/// see (V8, SpiderMonkey and JSC agree).
#[test]
fn async_from_sync_results_a_then_getter_changes() {
    let src = r#"
var log = [];
// A `then` getter that changes the iterator results it is looked up on
// (`value` 99 while not done) and keeps them, to change them later.
var kept = [];
function patch() {
  Object.defineProperty(Object.prototype, "then", { configurable: true, get() {
    if (Object.hasOwn(this, "done")) {
      log.push("then " + JSON.stringify(this));
      if (this.done === false) this.value = 99;
      kept.push(this);
    }
    return undefined;
  } });
}
function later() { for (const r of kept) { r.value = -1; r.done = true; } kept = []; }
function ticks(tag) { var p = Promise.resolve(); for (let i = 0; i < 3; i++) p = p.then(() => { log.push(tag + i); later(); }); }
async function main() {
  // (The case of the review: `[99,99]`.)
  Object.defineProperty(Object.prototype, "then", { configurable: true, get() { if (Object.hasOwn(this, "done") && this.done === false) this.value = 99; return undefined; } });
  const seen = []; for await (const v of [1, 2]) seen.push(v);
  log.push(JSON.stringify(seen));
  patch(); ticks("a");
  for await (var x of [1, 2]) log.push("for " + x);
  patch(); ticks("b");
  async function* g() { log.push("yield* gave " + (yield* [1, 2])); }
  for await (var y of g()) log.push("gen " + y);
  patch(); ticks("c");
  log.push("fromAsync " + JSON.stringify(await Array.fromAsync([1, 2])));
  // return() without a return(): yield*'s, and for await's break.
  var bare = { [Symbol.iterator]() { return { next() { return { value: 1, done: false }; } }; } };
  async function* h() { log.push("h gave " + (yield* bare)); }
  var it = h();
  log.push("h " + JSON.stringify(await it.next()));
  patch(); ticks("d");
  log.push("h " + JSON.stringify(await it.return(5)));
  patch(); ticks("e");
  for await (var z of bare) { log.push("body " + z); break; }
  log.push("broke");
  delete Object.prototype.then;
}
main().then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        r#"[99,99], a0, then {"value":1,"done":false}, a1, a2, b0, then {"value":1,"done":false}, b1, yield* gave -1, then {"done":true}, b2, c0, then {"value":1,"done":false}, c1, c2, fromAsync [], then {"value":1,"done":false}, then {"value":99,"done":false}, h {"value":99,"done":false}, d0, then {"value":5,"done":true}, d1, then {"value":-1,"done":true}, d2, h {"value":-1,"done":true}, e0, then {"value":1,"done":false}, e1, broke, e2"#
    );
}

/// `Array.fromAsync` steps a sync iterator as `for await` does: what the
/// step throws is a rejection, awaited, and doesn't close it; a mapper's
/// exception closes it by AsyncFromSyncIterator's `return()`, whose
/// promise is awaited (without a `return()` too).
#[test]
fn array_from_async_steps_a_sync_iterator_as_for_await() {
    let src = r#"
var log = [];
var p = Promise.resolve(); for (let i = 0; i < 6; i++) p = p.then(() => log.push("t" + i));
function S(tag, kind, ret) { var o = { [Symbol.iterator]() { return this; },
  next() { if (kind == "next") throw tag; if (kind == "obj") return 5;
    return { get done() { if (kind == "done") throw tag; return false; }, get value() { if (kind == "value") throw tag; return 1; } }; } };
  if (ret) o.return = function () { log.push("closed " + tag); return {}; };
  return o; }
for (const k of ["next", "obj", "done", "value"]) Array.fromAsync(S(k, k, true)).catch(e => log.push(typeof e == "string" ? e : e.name));
Array.fromAsync(S("m", "", true), () => { throw "m"; }).catch(e => log.push("mapper " + e));
Array.fromAsync(S("n", "", false), () => { throw "n"; }).catch(e => log.push("mapper " + e));
var q = Promise.resolve(); for (let i = 0; i < 10; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(
        out_after_jobs(src),
        "t0, t1, next, TypeError, done, value, closed m, t2, t3, mapper n, t4, mapper m, t5"
    );
}

/// An `await` whose PromiseResolve throws (a promise's `constructor`
/// getter) throws at the `await`, there and then: in a `for await`'s
/// close on an exception, the loop's exception goes on.
#[test]
fn awaits_whose_promise_resolve_throws_throw_there() {
    let src = r#"
var log = [];
function bad() { var p = Promise.resolve(1); Object.defineProperty(p, "constructor", { get() { throw "ctor"; } }); return p; }
var A = { [Symbol.asyncIterator]() { return { next() { return { value: 1, done: false }; }, return() { return bad(); } }; } };
(async () => { try { await bad(); } catch (e) { log.push("await " + e); } })();
(async () => { try { for await (var x of A) throw "body"; } catch (e) { log.push("throw " + e); } })();
(async () => { try { for await (var x of A) break; } catch (e) { log.push("break " + e); } })();
async function* h() {}
h().return(bad()).then(() => log.push("return fulfilled"), e => log.push("return " + e));
async function* g() { yield 1; }
var it = g(); it.next().then(() => it.return(bad())).then(() => log.push("yield fulfilled"), e => log.push("yield " + e));
var q = Promise.resolve(); for (let i = 0; i < 6; i++) q = q.then(() => 0);
q.then(() => { globalThis.out = log.join(", "); });
"#;
    assert_eq!(out_after_jobs(src), "await ctor, throw body, break ctor, return ctor, yield ctor");
}

/// The front end's public entry points take a file's text as it is: a
/// private use character a lone surrogate's mapping uses (U+F0000, the
/// `ESCAPE` U+F0800) or U+FDD0 in a string, template or regular
/// expression literal that `lex` and `lex_as` give is itself (its code
/// units in the value, the mapping's form), the tokens at the text's
/// offsets; `parse`'s error offsets and `compile`'s and a module's
/// columns count the text's characters, and so do `eval`'s.
#[test]
fn front_end_entry_points_take_text_as_it_is() {
    use cloison::js::lex::{Tok, lex, lex_as, to_units};
    for ch in ['\u{F0000}', '\u{F0800}', '\u{FDD0}'] {
        let c = ch.to_string();
        let units: Vec<u16> = c.encode_utf16().collect();
        for module in [false, true] {
            let src = format!("x = '{ch}'; y = `{ch}${{1}}{ch}`; z = /{ch}/u; w = \"a{ch}\\n\"");
            let toks = lex_as(&src, module).unwrap();
            let strs: Vec<Vec<u16>> = toks
                .iter()
                .filter_map(|t| match &t.tok {
                    Tok::Str(s) => Some(to_units(s)),
                    Tok::Template(p) => Some(to_units(p.cooked.as_deref().unwrap())),
                    Tok::RegExp(r) => Some(to_units(&r.0)),
                    _ => None,
                })
                .collect();
            let nl: Vec<u16> = units.iter().copied().chain([10]).collect();
            let a: Vec<u16> = [97].into_iter().chain(nl).collect();
            assert_eq!(strs, [units.clone(), units.clone(), units.clone(), units.clone(), a], "{src}");
            // (Each token at its offset in the text: `w` after the last.)
            let w = toks.iter().find(|t| t.tok == Tok::Ident("w".into())).unwrap();
            assert_eq!(w.pos as usize, src.find("w =").unwrap());
            assert_eq!(lex(&src).unwrap().len(), toks.len());
        }
        let bad = format!("var x = \"{ch}\"; x x");
        let e = cloison::js::parse(&bad).unwrap_err();
        assert_eq!(e.pos, bad.rfind('x').unwrap(), "{bad}");
        assert!(e.display(&bad).starts_with("1:16:"), "{}", e.display(&bad));
        assert!(compile_error(&bad).starts_with("1:16:"), "{}", compile_error(&bad));
        let dir = module_dir(&format!("text-columns-{}", ch as u32), &[("main.mjs", &bad)]);
        let m = run_module(&dir, "main.mjs").unwrap_err();
        assert!(m.contains(":1:16:"), "{m}");
        let js = format!("var m; try {{ eval('var x = \"{ch}\"; x x'); }} catch (e) {{ m = e.message; }} m");
        assert!(value(&js).starts_with("1:16:"), "{}", value(&js));
    }
}

/// `Regex::check` and `Regex::new` take a pattern's text (`&str`),
/// `check_units` and `of_units` its code units (a lone surrogate's its
/// own); the front end's entry points that took a `&str` still do, their
/// source code forms (`lex::Source`) under names of their own.
#[test]
fn text_entry_points_keep_their_signatures() {
    use cloison::regexp::Regex;
    assert!(Regex::check("a", "u").is_ok());
    assert!(Regex::check("[\\p{L}--[a-z]]", "v").is_ok());
    assert!(Regex::check("\u{1F600}{2}", "u").is_ok());
    assert!(Regex::check("(", "u").is_err());
    assert!(Regex::check("\\-", "u").is_err());
    assert!(Regex::check("\\-", "").is_ok());
    assert!(Regex::check("[a&&b]", "v").is_ok());
    assert!(Regex::check("[(]", "v").is_err());
    assert!(Regex::check("a", "uv").is_err());
    assert!(Regex::new("^\u{1F600}$", "u").unwrap().exec(&"\u{1F600}".encode_utf16().collect::<Vec<_>>(), 0).is_some());
    assert!(Regex::check_units(&[b'\\' as u16, 0xD83D], "u").is_err());
    assert!(Regex::check_units(&[b'\\' as u16, 0xD83D], "").is_ok());
    assert!(Regex::of_units(&[0xD83D], "").unwrap().exec(&[0xD83D], 0).is_some());
    // The front end's: a file's text in, its offsets out.
    let text = "var s = '\u{F0000}'; s s";
    assert!(cloison::js::is_one_function("(function () { return '\u{F0000}' })"));
    assert!(cloison::js::compile_script_lazy(text).unwrap_err().starts_with("1:16:"));
    assert!(cloison::js::compile_indirect_eval(text).unwrap_err().starts_with("1:16:"));
    assert!(cloison::js::compile_strict_eval("'\u{FDD0}'; x x").unwrap_err().starts_with("1:8:"));
    assert!(
        cloison::js::compile_direct_eval(text, false, false, false, false, "", true, &[])
            .unwrap_err()
            .starts_with("1:16:")
    );
    let arena = cloison::js::ast::Arenas::default();
    let e = cloison::js::parse::parse_source(text, &arena, false).err().unwrap();
    assert_eq!(e.pos, text.rfind('s').unwrap());
    let e = cloison::js::parse::parse_module("export var s = '\u{F0800}'; s s", &arena, 0).err().unwrap();
    assert_eq!(e.pos, "export var s = '\u{F0800}'; s s".rfind('s').unwrap());
    let p = cloison::js::parse::parse_source("var s = '\u{F0000}';", &arena, false).unwrap();
    assert!(cloison::js::codegen::generate(&p, "var s = '\u{F0000}';").is_ok());
}

/// The public parsers' offsets (statements', functions' and their text's)
/// are the text's, after a private use character a lone surrogate's
/// mapping uses (U+F0000, the `ESCAPE` U+F0800) or U+FDD0 too; functions'
/// `toString` (a file's, `eval`'s, lazily compiled or not) is their text.
#[test]
fn public_parsers_give_offsets_in_the_text() {
    use cloison::js::{ast::Arenas, parse};
    for ch in ['\u{F0000}', '\u{F0800}', '\u{FDD0}'] {
        let text = format!(
            "var a = '{ch}'; var r = /{ch}/u; var t = `{ch}${{a}}{ch}`;\nfunction f(x) {{ return '{ch}' + x }}\nclass C {{ m() {{ return /{ch}/ }} }}\nf(a);"
        );
        let check = |p: &cloison::js::ast::Program, top: bool| {
            let f = p.functions.iter().find(|g| g.name.as_deref() == Some("f")).expect("f");
            assert_eq!(
                &text[f.pos as usize..f.end as usize],
                format!("function f(x) {{ return '{ch}' + x }}"),
                "{text}"
            );
            assert_eq!(&text[f.params_pos as usize..f.end as usize], format!("(x) {{ return '{ch}' + x }}"));
            let m = p.functions.iter().find(|g| g.name.as_deref() == Some("m")).expect("m");
            assert_eq!(&text[m.params_pos as usize..m.end as usize], format!("() {{ return /{ch}/ }}"));
            if top {
                let starts: Vec<&str> = p.functions[0].body.iter().map(|s| &text[s.pos as usize..]).collect();
                for want in ["var a", "var r", "var t", "f(a);"] {
                    assert!(starts.iter().any(|s| s.starts_with(want)), "{want} in {text}");
                }
            }
        };
        let arena = Arenas::default();
        check(&parse::parse_source(&text, &arena, false).unwrap(), true);
        let arena = Arenas::default();
        check(&parse::parse_source(&text, &arena, true).unwrap(), true);
        let arena = Arenas::default();
        check(
            &parse::parse_source_as(&text, &arena, false, Some((false, "", false, true, false, false, false))).unwrap(),
            true,
        );
        let arena = Arenas::default();
        check(&parse::parse_module(&text, &arena, 0).unwrap(), false);
        // And the code made of it: `toString` the text, compiled now or
        // lazily (from `eval`).
        let fs = format!("function f(x) {{ return '{ch}' + x }}");
        let js = format!(
            "{text}\n[f.toString() === {fs:?}, C.prototype.m.toString() === {:?}, f(1).length].join()",
            format!("m() {{ return /{ch}/ }}")
        );
        assert_eq!(value(&js), format!("true,true,{}", ch.len_utf16() + 1));
        let e = format!(
            "var g = eval({:?}); [g.toString() === {fs:?}, g(1).length, eval({:?}).toString() === {fs:?}].join()",
            format!("(function () {{ var z = '{ch}'; return {fs}; }})()"),
            format!("({fs})")
        );
        assert_eq!(value(&e), format!("true,{},true", ch.len_utf16() + 1));
    }
}

/// `codegen::generate_lazy` takes a script's text, as `parse_source` does:
/// a function it leaves to compile later, with a U+F0000, U+F0800 or
/// U+FDD0 in its strings, regular expression and template, has them, and
/// its `toString` and `LazyFn::text_units` are its text.
#[test]
fn generate_lazy_takes_text() {
    use cloison::js::{ast::Arenas, codegen, parse};
    for ch in ['\u{F0000}', '\u{F0800}', '\u{FDD0}'] {
        let f = format!(
            "function f() {{ return ['{ch}', /{ch}/u.source, `{ch}`, String.raw`{ch}`].map(s => s.codePointAt(0)).join() }}"
        );
        let text = format!("var a = '{ch}'; {f}\n[f(), f.toString() === {f:?}, a.codePointAt(0)].join(' ')");
        let arena = Arenas::default();
        let p = parse::parse_source(&text, &arena, true).unwrap();
        let source: std::rc::Rc<str> = text.as_str().into();
        let m = codegen::generate_lazy(&p, 0, &source, &|pos| pos, &[]).unwrap();
        let lazy = m.functions.iter().find_map(|g| g.lazy.as_ref()).expect("a function left for later");
        assert_eq!(String::from_utf16(&lazy.text_units()).unwrap(), &f["function f".len()..]);
        let mut vm = Vm::new();
        // SAFETY: the front end's code, as `js::compile`'s.
        let r = vm.run(&unsafe { cloison::asm::Checked::new_unchecked(m) }).unwrap();
        let c = ch as u32;
        assert_eq!(display(r), format!("{c},{c},{c},{c} true {c}"));
    }
}

/// Warm code tiering up looks at the calls its function makes for ones
/// worth inlining (`jit::inlining_pays`): an array literal's elements are
/// calls with no cache, in a function that may have no other.
#[test]
fn warm_functions_with_array_literals_tier_up() {
    let src = r#"
function f(x, n) { for (var i = 0; i < n; i++) {} return [x]; }
function g(a) { return a + 1; }
function h(x, n) { var s = 0; for (var i = 0; i < n; i++) s = g(s); return [x, s, [x, x]]; }
var t = f(1, 2000)[0];
for (var k = 0; k < 300; k++) t += h(k, 50)[1] + f(k, 20)[0];
t + " " + JSON.stringify(h(7, 3));
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [1, 1000] {
        let mut vm = Vm::new();
        vm.set_warm_tier(true);
        vm.set_tier_back_edges(budget);
        let v = vm.run(&module).unwrap();
        assert_eq!(display(v), "59851 [7,3,[7,7]]", "tier at {budget}");
    }
}

#[test]
fn expressions() {
    // `!`, `!=`, `&&` and `||` as values, the conditional operator, the
    // comma operator and `void`.
    assert_eq!(
        value(
            "function f(a, b) {
               var n = 0;
               if (!(a < b)) n += 1;
               if (a != b) n += 2;
               n += (a && b) == b ? 4 : 0;
               n += (0 || b) == b ? 8 : 0;
               n += (null || 0) == 0 ? 16 : 0;
               n += (a < b) == (b != a) ? 32 : 0;
               return (void 0, n);
             }
             f(3, 2);"
        ),
        "31"
    );
    // Compound assignments and increments of variables, fields, elements
    // and globals; array literals.
    assert_eq!(
        value(
            "var g = 5;
             function P() { this.x = 1; }
             function f() {
               var p = new P(), a = [1, 2, 3], i = 1;
               p.x += 10; p.x |= 4; ++p.x; p.x--;
               a[i] += 5; a[i + 1]++; --a[0];
               g += 2; g++;
               i -= 1; i %= 7;
               return p.x + a[0] + a[1] + a[2] + g + i;
             }
             f();"
        ),
        "34"
    );
    // A left operand used as a register is copied when the right one
    // assigns it.
    assert_eq!(value("function f() { var x = 1; return x + (x = 10); } f();"), "11");
    assert_eq!(value("function f() { var x = 1; x += (x = 10); return x; } f();"), "11");
    // Nested functions that capture nothing, function expressions,
    // methods and `undefined`.
    assert_eq!(
        value(
            "function O() {}
             function f() {
               function twice(v) { return v + v; }
               var h = function (k) { return k + 1; };
               var o = new O();
               o.m = function () { return this.v; };
               o.v = 7;
               return h(twice(2)) + o.m() + (undefined == null ? 100 : 0);
             }
             f();"
        ),
        "112"
    );
}

/// An uncaught `throw` ends the run with the value thrown, from the script,
/// from a function, and from a function compiled by then (where `Throw`
/// leaves for the interpreter).
#[test]
fn uncaught_throws_end_the_run() {
    for (src, want) in [
        ("1; throw \"a\"; 2;", "a"),
        ("throw 1.5;", "1.5"),
        ("function f() { throw null; } f();", "null"),
        (
            "function f(n) { if (n > 5) throw \"big \" + n; return n; } \
             var s = 0; for (var i = 0; i < 5000; i++) s += f(i & 3); f(9);",
            "big 9",
        ),
        (
            "function f(o) { for (var i = 0; i < 3000; i++) if (i == o.at) throw o; return 0; } \
             f({ at: -1 }); f({ at: 2500 });",
            "[object Object]",
        ),
    ] {
        assert_eq!(run(src), Err(want.to_string()), "{src}");
    }
}

/// The group 2 operators (`-`, `*`, `/`, shifts, `>`, `<=`, `>=`, `===`,
/// tests against null and undefined) at their edges: int32 overflow, -0
/// products, `>>> 0` of negatives, NaN, an int32 equal to a double. The
/// expected values are Node's.
const GROUP2: &str = r#"
function arith(n) {
  var s = 0, d = 0, i, a, b;
  for (i = 0; i < n; i++) {
    a = (i * 7919) % 2001 - 1000;
    b = (i * 104729) % 41 - 20;
    s = (s + a * b) | 0;
    s = (s + (a - b) * 3) | 0;
    s = (s - (b - a)) | 0;
    s = (s + (a << 3) + (a >> 2) + (a >>> 28)) | 0;
    if (b != 0) d = d + a / b;
    d = d + a / 8 - b * 2;
  }
  return s + d;
}
function overflow() {
  var big = 2147483647, x = 0, i, r = 0;
  for (i = 0; i < 50; i++) {
    x = big - i;
    r = r + (x + 10) - (0 - x - 5) + x * 3 + (x - (0 - big));
  }
  return r;
}
function negzero() {
  var i, z, n = 0;
  for (i = 0; i < 40; i++) {
    z = (i % 3 - 1) * 0;
    if (1 / z < 0) n = n + 1;
    z = 0 * (i % 4 - 2);
    if (1 / z < 0) n = n + 10;
    z = (i % 2) * -5;
    if (1 / z < 0) n = n + 100;
  }
  return n;
}
function shifts() {
  var i, t = 0, v;
  for (i = -30; i < 30; i++) {
    v = i * 123457;
    t = t + (v >>> 0) + (v >>> 1) + (v >> 31) + (v << 31) + (v << 0) + (v >> 0);
  }
  return t;
}
function rel(xs, ys) {
  var c = 0, i, j, x, y;
  for (i = 0; i < xs.length; i++)
    for (j = 0; j < ys.length; j++) {
      x = xs[i]; y = ys[j];
      if (x > y) c = c + 1;
      if (x >= y) c = c + 3;
      if (x <= y) c = c + 7;
      if (x < y) c = c + 11;
      if (x === y) c = c + 13;
      if (x !== y) c = c + 17;
      if (!(x > y)) c = c + 19;
      if (!(x <= y)) c = c + 23;
      c = c + (x > 2 ? 29 : 0) + (x >= 2 ? 31 : 0) + (x <= 2 ? 37 : 0);
      c = c + ((x > y) == true ? 41 : 0) + ((x <= y) == true ? 43 : 0);
    }
  return c;
}
function loops(n) {
  var c = 0, i, k;
  for (i = n; i >= 0; i -= 3) c = c + i;
  for (i = 1; i <= n; i++) c = c + 1;
  k = n;
  while (k > 5) { k = k - 2; c = c + k; }
  for (i = 0; i <= 2147483646; i += 1000000000) c = c + 1;
  return c;
}
function nulls() {
  var vs = [null, undefined, 0, false, vs, 3], c = 0, i, v, o = new O();
  for (i = 0; i < 60; i++) {
    v = vs[i % 6];
    if (v === undefined) c = c + 1;
    if (v === null) c = c + 10;
    if (v == null) c = c + 100;
    if (v != null) c = c + 1000;
    if (v !== undefined) c = c + 10000;
    if (o == null) c = c + 100000;
    if (i == null) c = c + 1000000;
    c = c + (v === null ? 2 : 0) + (v == undefined ? 4 : 0);
  }
  return c;
}
function O() { this.k = 1; }
var nan = 0 / 0;
"#;

#[test]
fn group2_operators_agree_with_node() {
    for (call, want) in [
        ("arith(3000)", "178913.9690706911"),
        ("overflow()", "751619269850"),
        ("negzero()", "4214"),
        ("shifts()", "128836055850"),
        ("rel([1, 2, 3, 5 / 2, nan, 0 * -1, 0], [2, 0, 5 / 2, nan])", "3400"),
        ("loops(1000)", "417668"),
        ("nulls()", "433260"),
    ] {
        assert_eq!(value(&format!("{GROUP2}\n{call};")), want, "{call}");
    }
}

/// Strings: concatenation with numbers of every format, `length`, `==` and
/// `===` between distinct strings and with numbers, `<` on strings,
/// ToBoolean, numeric strings in arithmetic, arrays as strings, strings in
/// fields. The expected values are Node's.
const STRINGS: &str = r#"
function concat() {
  var s = "", i, xs = [0, -1, 7, 2147483647, 0.5, -2.25, 1e21, 1.5e-7, 123.456, 1 / 3, 0 / 0, 1 / 0, -1 / 0, 0 * -1, 100, 1e300 * 10, 4294967296, 0.000001, 123456789012345680000, Math.pow(2, -25), Math.pow(2, -24), -2038828675386191.25, 1703588698511932.25, 9007199254740993, 5e-324, 0.1 + 0.2];
  for (i = 0; i < xs.length; i++) s = s + "|" + xs[i];
  return s;
}
function lengths() {
  var n = 0, i, s;
  for (i = 0; i < 1000; i++) { s = "ab" + i; n += s.length; }
  return n + "/" + "".length + "/" + "héllo".length + "/" + "é中".length;
}
function equality() {
  var c = 0, i, a, b, n;
  for (i = 0; i < 300; i++) {
    a = "x" + (i % 3); b = "x" + (i % 2); n = "" + (i % 4);
    if (a === b) c += 1;
    if (a == b) c += 10;
    if (a !== b) c += 100;
    if (n == i % 4) c += 1000;
    if (n === i % 4) c += 10000;
    if ("1" == 1) c += 100000;
    if ("" == 0) c += 1000000;
  }
  return c;
}
function relations() {
  var c = 0, i, ws = ["b", "a", "ab", "", "B", "10", "9"];
  for (i = 0; i < 49; i++) {
    var x = ws[i % 7], y = ws[(i / 7) | 0];
    if (x < y) c += 1;
    if (x <= y) c += 10;
    if (x > y) c += 100;
    if (x >= y) c += 1000;
  }
  return c;
}
function truthy() {
  var c = 0, i, vs = ["", "0", "a", " "];
  for (i = 0; i < 400; i++) { if (vs[i % 4]) c++; c += (vs[i % 4] || "z").length; }
  return c;
}
function numbers() {
  var t = 0, i, ss = ["12", " 7 ", "", "0x10", "1e3", "-2.5", "abc", "Infinity", ".5", "5.", "0b11", "1_0"];
  for (i = 0; i < ss.length; i++) t = t + "," + (ss[i] * 1) + (ss[i] - 0);
  return t + "|" + ("3" * "4") + "|" + ("8" / "2") + "|" + ([1, 2] + "") + "|" + ("a" + [3, [4, 5]]) + "|" + (1 + 2 + "3" + 4 + 5);
}
function fields() {
  function P(n) { this.name = n; this.k = n.length; }
  var t = 0, i, p;
  for (i = 0; i < 500; i++) { p = new P("p" + (i % 10)); t += p.k; if (p.name === "p3") t += 100; }
  return t;
}
"#;

#[test]
fn strings_agree_with_node() {
    for (call, want) in [
        (
            "concat()",
            "|0|-1|7|2147483647|0.5|-2.25|1e+21|1.5e-7|123.456|0.3333333333333333|NaN|Infinity|-Infinity|0|100|1e+301|4294967296|0.000001|123456789012345680000|2.9802322387695312e-8|5.960464477539063e-8|-2038828675386191.2|1703588698511932.2|9007199254740992|5e-324|0.30000000000000004",
        ),
        ("lengths()", "4890/0/5/2"),
        ("equality()", "330321100"),
        ("relations()", "30401"),
        ("truthy()", "700"),
        (
            "numbers()",
            "0,1212,77,00,1616,10001000,-2.5-2.5,NaNNaN,InfinityInfinity,0.50.5,55,33,NaNNaN|12|4|1,2|a3,4,5|3345",
        ),
        ("fields()", "6000"),
    ] {
        assert_eq!(value(&format!("{STRINGS}\n{call};")), want, "{call}");
    }
}

/// Object literals: empty, nested, with function, string and null values,
/// keywords and quoted keys as names, a later duplicate key winning, two
/// key orders (two shapes) at one site, identity under `==` and `===`. The
/// expected values are Node's.
const OBJECTS: &str = r#"
function literals() {
  var t = 0, i, o, e;
  for (i = 0; i < 300; i++) {
    e = {};
    o = { a: i, b: i * 2, c: { d: i & 7 }, s: "x" + i, f: function (k) { return this.a + k; }, a2: null };
    o.extra = 1;
    t += o.a + o.b + o.c.d + o.s.length + o.f(3) + (o.a2 == null ? 1 : 0) + o.extra + (e.a === undefined ? 1 : 0);
  }
  return t;
}
function duplicates() {
  var o = { k: 1, j: 2, k: 3 }, n = 0, i;
  for (i = 0; i < 100; i++) n += o.k * 10 + o.j;
  return n;
}
function shapes() {
  var xs = [], i, s = 0;
  for (i = 0; i < 200; i++) xs.push(i % 2 ? { x: i, y: 1 } : { y: 2, x: i });
  for (i = 0; i < 200; i++) s += xs[i].x * xs[i].y;
  return s;
}
function identity() {
  var a = {}, b = {}, c = a, n = 0, i;
  for (i = 0; i < 100; i++) { if (a == c) n++; if (a != b) n += 10; if (a === b) n += 100; }
  return n;
}
function keywords() {
  var o = { if: 1, default: 2, "quoted key": 3, $d: 4, _u: 5 };
  return o.if + o.default + o.$d + o._u;
}
"#;

#[test]
fn object_literals_agree_with_node() {
    for (call, want) in [
        ("literals()", "183332"),
        ("duplicates()", "3200"),
        ("shapes()", "29800"),
        ("identity()", "1100"),
        ("keywords()", "12"),
    ] {
        assert_eq!(value(&format!("{OBJECTS}\n{call};")), want, "{call}");
    }
}

/// `Object.defineProperty`: read-only properties, own and inherited, ignore
/// assignments (also where the JIT knows the shape); `writable: true`;
/// redefining a property's attributes and value; a cached miss invalidated
/// when the property appears on the prototype; the target is returned.
/// The expected values are Node's.
const DEFINE: &str = r#"
function readOnly() {
  var o = {}, i, n = 0;
  Object.defineProperty(o, "k", { value: 5 });
  for (i = 0; i < 100; i++) { o.k = i; n += o.k; }
  return n;
}
function inheritedReadOnly() {
  function P() {}
  Object.defineProperty(P.prototype, "ro", { value: 7 });
  var n = 0, i, p;
  for (i = 0; i < 100; i++) { p = new P(); p.ro = i; n += p.ro; }
  return n;
}
function writable() {
  var o = {}, i, n = 0;
  Object.defineProperty(o, "w", { value: 1, writable: true });
  for (i = 0; i < 100; i++) { o.w = o.w + 1; n += o.w; }
  return n;
}
function redefine() {
  var o = { a: 1 }, n = 0, i;
  Object.defineProperty(o, "a", { writable: false });
  for (i = 0; i < 50; i++) { o.a = 9; n += o.a; }
  Object.defineProperty(o, "a", { value: 3, writable: true });
  for (i = 0; i < 50; i++) { o.a = o.a + 1; n += o.a; }
  return n;
}
function missesThenDefined() {
  function Q() { this.x = 1; }
  var q = new Q(), n = 0, i;
  for (i = 0; i < 200; i++) { if (q.later === undefined) n++; }
  Object.defineProperty(Q.prototype, "later", { value: 10 });
  for (i = 0; i < 200; i++) { if (q.later === undefined) n++; else n += q.later; }
  Q.prototype.other = 2;
  for (i = 0; i < 200; i++) n += q.other;
  return n;
}
function returnsTarget() {
  var o = {};
  return Object.defineProperty(o, "z", { value: 4 }) === o ? o.z : -1;
}
"#;

#[test]
fn define_property_agrees_with_node() {
    for (call, want) in [
        ("readOnly()", "500"),
        ("inheritedReadOnly()", "700"),
        ("writable()", "5150"),
        ("redefine()", "1475"),
        ("missesThenDefined()", "2600"),
        ("returnsTarget()", "4"),
    ] {
        assert_eq!(value(&format!("{DEFINE}\n{call};")), want, "{call}");
    }
}

/// `switch`: fall-through, `default` first or in the middle, string cases,
/// a double equal to an int32 case (and `"1"` and -0 against them), case
/// expressions evaluated in order only until one matches, the discriminant
/// evaluated once, `break` and `continue` in loops, labeled `break`, nested
/// and empty switches. The expected values are Node's.
const SWITCH: &str = r#"
function fallthrough() {
  var t = 0, i;
  for (i = 0; i < 60; i++) {
    switch (i % 6) {
      case 0: t += 1;
      case 1: t += 10; break;
      default: t += 100;
      case 4: t += 1000; break;
      case 5:
    }
  }
  return t;
}
function defaultFirst() {
  var t = 0, i;
  for (i = 0; i < 30; i++) {
    switch (i % 5) { default: t += 7; break; case 2: t += 2; case 3: t += 3; }
  }
  return t;
}
function strings() {
  var ws = ["a", "b", "c", "ab"], t = "", i;
  for (i = 0; i < 8; i++) {
    switch (ws[i % 4]) { case "a": t += "A"; break; case "a" + "b": t += "X"; break; case "c": t += "C"; break; default: t += "?"; }
  }
  return t;
}
function numbers() {
  var vs = [1, 2.5, 5 / 5, "1", 0 * -1, 0, null, undefined], t = 0, i;
  for (i = 0; i < 80; i++) {
    switch (vs[i % 8]) { case 0: t += 1; break; case 1: t += 10; break; case 2.5: t += 100; break; case null: t += 1000; break; default: t += 10000; }
  }
  return t;
}
var log = 0, evals = 0;
function c(k) { log = log * 10 + k; return k; }
function d(v) { evals++; return v; }
function lazyCases() {
  var hits = 0, i;
  log = 0;
  for (i = 0; i < 4; i++) {
    switch (i) { case c(1): hits += 1; break; case c(2): hits += 2; break; case c(3): hits += 3; }
  }
  return log + "/" + hits;
}
function onceAndControl() {
  var t = 0, i, j;
  evals = 0;
  outer: for (i = 0; i < 10; i++) {
    switch (d(i % 4)) {
      case 0: continue;
      case 1: for (j = 0; j < 5; j++) { if (j == 2) break; t += 1; } break;
      case 2: switch (i) { case 2: t += 100; break; default: t += 1000; } break;
      case 3: if (i > 6) break outer; t += 10;
    }
    t += 10000;
  }
  switch (t) {}
  return evals + "/" + t;
}
"#;

#[test]
fn switch_agrees_with_node() {
    for (call, want) in [
        ("fallthrough()", "32210"),
        ("defaultFirst()", "174"),
        ("strings()", "A?CXA?CX"),
        ("numbers()", "211220"),
        ("lazyCases()", "123112123/6"),
        ("onceAndControl()", "8/51114"),
    ] {
        assert_eq!(value(&format!("{SWITCH}\n{call};")), want, "{call}");
    }
}

/// `for … in`: own properties in insertion order, then inherited
/// enumerable ones, a non-enumerable property hiding an inherited one of
/// its name, built-ins never enumerated; array indices (as strings) before
/// named properties, a string's indices, nothing for null, undefined or a
/// number; `break`, `continue`, labeled `continue`; keys as strings, used
/// as `o[k]`. The expected values are Node's.
const FOR_IN: &str = r#"
function P() { this.own1 = 1; this.own2 = 2; }
P.prototype.inh = 3;
Object.defineProperty(P.prototype, "hidden", { value: 4 });
var log = "";
function order() {
  var p = new P(), s = "", k, n = 0, i;
  p.later = 5;
  for (i = 0; i < 50; i++) { s = ""; for (k in p) s += k + ";"; n += s.length; }
  return s + "/" + n;
}
function shadowing() {
  var o = Object.create ? 0 : 0, s = "", k;
  function Q() {}
  Q.prototype.a = 1; Q.prototype.b = 2;
  var q = new Q();
  Object.defineProperty(q, "a", { value: 9 });
  for (k in q) s += k;
  return s;
}
function arraysAndStrings() {
  var a = [10, 20, 30], s = "", k, t = 0, i;
  a.extra = 1;
  for (k in a) s += k + ",";
  for (k in "xyz") s += k;
  for (k in null) s += "N";
  for (k in undefined) s += "U";
  for (k in 5) s += "5";
  for (i = 0; i < 100; i++) for (k in a) t += a[k] === undefined ? 0 : 1;
  return s + "/" + t;
}
function control() {
  var o = { a: 1, b: 2, c: 3, d: 4 }, s = "", k, j;
  for (k in o) { if (k == "b") continue; if (k == "d") break; s += k; }
  outer: for (j = 0; j < 3; j++) for (k in o) { if (k == "c") continue outer; s += j; }
  return s;
}
function keysAreStrings() {
  var a = [7, 8], t = "", k;
  for (k in a) t += (k + 1) + "|";
  for (g in { x: 1 }) t += g;
  return t;
}
var g;
"#;

#[test]
fn for_in_agrees_with_node() {
    for (call, want) in [
        ("order()", "own1;own2;later;inh;/1000"),
        ("shadowing()", "b"),
        ("arraysAndStrings()", "0,1,2,extra,012/400"),
        ("control()", "ac001122"),
        ("keysAreStrings()", "01|11|x"),
    ] {
        assert_eq!(value(&format!("{FOR_IN}\n{call};")), want, "{call}");
    }
}

/// Polymorphic accesses, compiled: loads, stores and transitions on
/// receivers of two to five shapes (fields at different offsets, in and
/// out of the object, a prototype's method, a missing one), arrays whose
/// elements change type, and shapes appearing after compilation (an exit).
/// The expected values are Node's.
const POLY: &str = r#"
function A(x) { this.x = x; this.y = 1; }
function B(x) { this.k = 0; this.y = 2; this.x = x; }
function C(x) { this.y = 3; this.z = 0; this.w = 0; this.x = x; }
function D(x) { this.x = x; }
function E(x) { this.q = 1; this.r = 2; this.x = x; }
A.prototype.m = function () { return 1; };
B.prototype.m = function () { return 2; };
function F() {}
F.prototype = A.prototype;
function make(i, kinds) {
  var k = i - ((i / kinds) | 0) * kinds;
  return k == 0 ? new A(i) : k == 1 ? new B(i) : k == 2 ? new C(i) : k == 3 ? new D(i) : new E(i);
}
function run(kinds, n) {
  var os = [], i, r, s = 0;
  for (i = 0; i < 40; i++) os.push(make(i, kinds));
  for (r = 0; r < n; r++) {
    for (i = 0; i < os.length; i++) {
      var o = os[i];
      s = (s + o.x + (o.y | 0)) | 0;
      o.x = o.x + 1;
      if (r == 5) o.tag = i;
      if (o.tag != undefined) s = (s + o.tag) | 0;
      if (o.m != undefined) s = (s + o.m()) | 0;
    }
  }
  return s;
}
function grow() {
  var out = 0, r, i;
  for (r = 0; r < 300; r++) {
    var a = [], b = [1.5];
    for (i = 0; i < 20; i++) { a.push(i); b.push(i); }
    out = (out + a.length + b.length + a[3] + b[1]) | 0;
  }
  return out;
}
function late() {
  var os = [], s = 0, i, r;
  for (i = 0; i < 30; i++) os.push((i & 1) ? new A(i) : new B(i));
  for (r = 0; r < 2000; r++) {
    if (r == 1500) os[7] = new C(7);
    if (r == 1700) os[8] = new F();
    for (i = 0; i < os.length; i++) s = (s + (os[i].x | 0)) | 0;
  }
  return s;
}
"#;

#[test]
fn polymorphic_accesses_agree_with_node() {
    for (call, want) in [
        ("run(2, 1500)", "47486100"),
        ("run(3, 1500)", "47484600"),
        ("run(4, 1500)", "47441100"),
        ("run(5, 1500)", "47414100"),
        ("grow()", "13200"),
        ("late()", "867600"),
    ] {
        assert_eq!(value(&format!("{POLY}\n{call};")), want, "{call}");
    }
    // All in one run: the caches carry over.
    assert_eq!(value(&format!("{POLY}\nrun(2, 300) + run(3, 300) + run(4, 300) + run(5, 300);")), "9152700");
}

/// Polymorphic calls, inlined: two to five callees with different
/// parameter counts (missing arguments, locals that start undefined), one
/// calling further, callees appearing after compilation, a function made
/// anew at each call, and methods replaced after compilation. The expected values are Node's.
const POLY_CALLS: &str = r#"
function K1(v) { this.v = v; }
K1.prototype.f = function (a) { return this.v + a; };
function K2(v) { this.w = 0; this.v = v; }
K2.prototype.f = function (a, b, c) { var t; if (t == undefined) t = 1; return this.v * 2 + (b == undefined ? 5 : b) + t; };
function K3(v) { this.v = v; }
K3.prototype.f = function () { var x, y; x = this.v; return x - (y == undefined ? 1 : y); };
function K4(v) { this.v = v; }
K4.prototype.helper = function (a) { return a * 3; };
K4.prototype.f = function (a) { return this.helper(a) + this.v; };
function K5(v) { this.v = v; }
K5.prototype.f = function (a) { return 0 - a; };
function make(i, kinds) {
  var k = i - ((i / kinds) | 0) * kinds;
  return k == 0 ? new K1(i) : k == 1 ? new K2(i) : k == 2 ? new K3(i) : k == 3 ? new K4(i) : new K5(i);
}
function run(kinds, n) {
  var os = [], i, r, s = 0;
  for (i = 0; i < 30; i++) os.push(make(i, kinds));
  for (r = 0; r < n; r++)
    for (i = 0; i < os.length; i++) s = (s + os[i].f(r)) | 0;
  return s;
}
function later() {
  var os = [], i, r, s = 0;
  for (i = 0; i < 20; i++) os.push((i & 1) ? new K1(i) : new K3(i));
  for (r = 0; r < 3000; r++) {
    if (r == 2000) os[4] = new K2(4);
    if (r == 2500) os[6] = new K5(6);
    for (i = 0; i < os.length; i++) s = (s + os[i].f(r, i)) | 0;
  }
  return s;
}
function fresh() {
  var s = 0, i;
  for (i = 0; i < 3000; i++) {
    var g = function (x) { return x + 1; };
    s = (s + g(i)) | 0;
  }
  return s;
}
function swap() {
  var os = [], i, r, s = 0;
  for (i = 0; i < 20; i++) os.push((i & 1) ? new K1(i) : (i & 2) ? new K3(i) : new K2(i));
  for (r = 0; r < 4000; r++) {
    if (r == 2000) K1.prototype.f = function (a) { return a * 7; };
    if (r == 3000) { K3.prototype.f = K1.prototype.f; os[1].f = function () { return 1000; }; }
    for (i = 0; i < os.length; i++) s = (s + os[i].f(r, i)) | 0;
  }
  return s;
}
"#;

#[test]
fn polymorphic_calls_agree_with_node() {
    for (call, want) in [
        ("run(2, 2000)", "31485000"),
        ("run(3, 2000)", "21250000"),
        ("run(4, 2000)", "59163000"),
        ("run(5, 2000)", "36876000"),
        ("later()", "44157750"),
        ("fresh()", "4501500"),
        ("run(2, 300) + run(4, 300) + run(5, 300) + later()", "47476350"),
    ] {
        assert_eq!(value(&format!("{POLY_CALLS}\n{call};")), want, "{call}");
    }
}

/// Compiled element stores, `push` and `pop`, and inlined constructors:
/// stores of ints, doubles and objects (a double into ints, past the
/// length), pushes that grow the elements and pops down to empty, and a
/// constructor chain through `.call` whose add overflows (an exit inside
/// the inlined `new`). The expected values are Node's.
const ARRAYS: &str = r#"
function stores() {
  var a = [], d = [0.5], o = [], s = 0, i, r;
  for (i = 0; i < 50; i++) { a.push(i); d.push(i); o.push(null); }
  for (r = 0; r < 300; r++) {
    for (i = 0; i < 50; i++) { a[i] = a[i] + r; d[i] = d[i] * 0.5 + r; }
    if (r == 150) { a[3] = 2.5; o[7] = a; }
    a[60] = r;
  }
  for (i = 0; i < 50; i++) s += a[i] + d[i];
  return s + a.length + (o[7] === a ? 1 : 0);
}
function stack() {
  var st = [], n = 0, i, r, v;
  for (r = 0; r < 400; r++) {
    for (i = 0; i < (r & 15); i++) st.push(i * r);
    while (st.length > 3) { v = st.pop(); n = (n + v) | 0; }
    if (st.pop() == undefined) n++;
  }
  var e = [];
  return n + (e.pop() === undefined ? 1 : 0) + st.length;
}
function P(x, y) { this.x = x; this.y = y + 1; }
function Q(a, b) { P.call(this, a, b); this.z = a * b; }
function ctors() {
  var s = 0, i, o;
  for (i = 0; i < 3000; i++) {
    o = new Q(i, i < 2500 ? i : 2147483647);
    s = (s + o.x + o.z) | 0;
    if (o.y > 3000) s = (s + 1) | 0;
  }
  return s;
}
function add(c, v) { return c.push(v); }
function take(c) { return c.pop(); }
function mixed() {
  var s = 0, i, a, b, other = { n: 0, push: function (v) { this.n += v; return -1; } };
  for (i = 0; i < 3000; i++) {
    a = [];
    b = [i, 0.5];
    s += add(a, i) + add(b, i & 7) + (add(b, "x") > 3 ? 1 : 0);
    if (i > 2000) s += add(other, 2);
    if (i % 500 == 0) { a.pop(); s += take(a) === undefined ? 1 : 0; }
    s += take(b) === "x" ? 1 : 0;
  }
  return s + other.n;
}
"#;

#[test]
fn arrays_and_constructors_agree_with_node() {
    for (call, want) in
        [("stores()", "2262261.5"), ("stack()", "2847651"), ("ctors()", "913365704"), ("mixed()", "19005")]
    {
        assert_eq!(value(&format!("{ARRAYS}\n{call};")), want, "{call}");
    }
}

/// A global loaded several times in compiled straight-line code, changed
/// in between by stores (`g = …`), by calls that store it, inlined or not
/// (`swap`, `setG`), while its object is stored to through it. The
/// expected values are Node's.
const GLOBALS: &str = r#"
var g = { v: 1 }, h = { v: 100 }, other = { v: 7 };
function swap() { var t = g; g = h; h = t; }
function setG(o) { g = o; }
function loads() {
  var s = 0, i;
  for (i = 0; i < 3000; i++) {
    s += g.v;
    if ((i & 7) == 0) swap();
    s += g.v;
    if ((i & 15) == 1) g = other;
    s += g.v;
    if ((i & 31) == 2) setG(h);
    s += g.v;
    if ((i & 63) == 3) { g = h; h = other; }
    s += g.v;
    g.v = g.v + 1;
    s = s | 0;
  }
  return s + g.v;
}
"#;

#[test]
fn repeated_global_loads_agree_with_node() {
    assert_eq!(value(&format!("{GLOBALS}\nloads();")), "22481141");
}

/// Scalar replacement: objects of inlined constructors that are only read
/// from aren't allocated, and exits while one is live (an int32 add
/// overflowing, a field getting a double) allocate it; objects from two
/// constructors joining, and one escaping now and then. The expected
/// values are Node's.
const SCALARS: &str = r#"
function V(x, y) { this.x = x; this.y = y; }
function W(a) { this.a = a; this.b = a + 1; }
var kept = [];
function plain() {
  var s = 0, i;
  for (i = 0; i < 5000; i++) { var v = new V(i, 2 * i); s = (s + v.x + v.y) | 0; }
  return s;
}
function overflow() {
  var s = 0, i;
  for (i = 0; i < 3000; i++) {
    var v = new V(i, i < 2000 ? i : 2147483000 + i);
    var t = v.y + 1000;
    s = (s + v.x + (t > 2147483647 ? 1 : 0)) | 0;
  }
  return s;
}
function doubles() {
  var s = 0, i;
  for (i = 0; i < 3000; i++) {
    var v = new V(i, i < 2500 ? i : i + 0.5);
    s += v.y - v.x;
  }
  var w = new V(1, 2);
  return s + w.y;
}
function branches() {
  var s = 0, i, o;
  for (i = 0; i < 3000; i++) {
    if (i & 1) o = new V(i, 1); else o = new W(i);
    s = (s + (o.x === undefined ? o.a + o.b : o.x + o.y)) | 0;
  }
  return s;
}
function sometimes() {
  var s = 0, i;
  for (i = 0; i < 3000; i++) {
    var v = new V(i, 0 - i);
    if (i % 1000 == 999) kept.push(v);
    s = (s + v.x + v.y + 1) | 0;
  }
  return s + kept.length + kept[1].x;
}
"#;

#[test]
fn scalar_replaced_objects_agree_with_node() {
    for (call, want) in [
        ("plain()", "37492500"),
        ("overflow()", "4499500"),
        ("doubles()", "252"),
        ("branches()", "6750000"),
        ("sometimes()", "5002"),
    ] {
        assert_eq!(value(&format!("{SCALARS}\n{call};")), want, "{call}");
    }
}

/// An object scalar replaced together with the object in its field: an
/// exit (the cold branch) allocates both, the inner one first. The
/// expected value is Node's.
const NESTED: &str = r#"
function Inner() { this.x = 7; }
function Outer(v) { this.v = v; this.inner = new Inner(); }
function f(n, cold, q) {
  var s = 0;
  for (var i = 0; i < n; i++) {
    var o = new Outer(i);
    if (i == cold) s += q.zz;
    s += o.inner.x;
  }
  return s;
}
var t = 0;
for (var r = 0; r < 300; r++) t += f(100, -1, null);
t + f(100, 50, { zz: 1 });
"#;

#[test]
fn nested_scalar_replaced_objects_agree_with_node() {
    assert_eq!(value(NESTED), "210701");
}

/// Arrays whose first elements come from several classes, so of shapes
/// differing in their elements' type alone: `push` is one case for them all
/// (see `Shape::family`), and takes the classes that come after it was
/// compiled (`main(5)`) too. The expected value is Node's.
const MIXED_ARRAYS: &str = r#"
function A(v) { this.v = v; }
function B(v) { this.w = 0; this.v = v; }
function C(v) { this.v = v * 2; }
function D(v) { this.x = 1; this.v = v; }
function E(v) { this.v = 0 - v; }
function sum(a) { var s = 0; for (var i = 0; i < a.length; i++) s += a[i].v; return s; }
function make(k, r) {
  var out = [];
  out.push(k == 0 ? new A(r) : k == 1 ? new B(r) : k == 2 ? new C(r) : k == 3 ? new D(r) : new E(r));
  out.push(new A(1));
  if (r % 7 == 0) out.push(3);
  return out;
}
function main(kinds) {
  var t = 0, k = 0;
  for (var r = 0; r < 20000; r++) {
    var a = make(k, r);
    if (++k == kinds) k = 0;
    t += sum(a.length > 2 ? [a[0], a[1]] : a) + a.length;
  }
  return t;
}
"#;

#[test]
fn arrays_first_filled_with_several_classes_agree_with_node() {
    assert_eq!(value(&format!("{MIXED_ARRAYS}\nmain(3) + main(5);")), "426748383");
}

/// Tests of values known where they are (see `fold_constant_tests`):
/// parameters inlined calls leave `undefined`, constants returned from
/// inlined calls, and values set on one path only, which a merge makes
/// unknown. The expected value is Node's.
const KNOWN_TESTS: &str = r#"
function or0(a, b) { return b || 0; }
function and1(a, b) { return (b && 1) || 0; }
function not(a, b) { return !b ? 3 : 4; }
function pick(a, b, c) { return c ? b : a; }
function zero() { return 0; }
function main() {
  var s = 0, flag = null, k = 0;
  for (var i = 0; i < 3000; i++) {
    s += or0(1) + or0(1, i) + and1(2) + and1(2, i & 1) + not(0) + not(0, i);
    s += pick(5, 7) + pick(5, 7, 1) + pick(5, 7, i & 2);
    if (zero()) s += 1000;
    if (flag) s += 100; else flag = i & 4;
    k = i & 1 ? 0 : 1;
    if (k) s += 9;
  }
  return s;
}
"#;

#[test]
fn tests_of_known_values_agree_with_node() {
    assert_eq!(value(&format!("{KNOWN_TESTS}\nmain();")), "4887999");
}

/// A captured boolean set to a constant and tested after int arithmetic,
/// which runs nothing, so `fold_captured_tests` folds the test: the
/// result is still the interpreter's, and the compiled loop tests nothing
/// but `i < n`, with no `else` arm (#64).
#[test]
fn tests_of_captured_registers_fold_across_typed_arithmetic() {
    let src = r#"
function run(n) {
  var flag = false, s = 0;
  var peek = function () { return flag; };
  for (var i = 0; i < n; i++) {
    flag = true;
    s += i & 7;
    if (flag) s += 1; else s = (s * 3) | 0;
    flag = false;
    s ^= i;
    if (!flag) s += 2;
  }
  return s + (peek() ? 1 : 0);
}
var t = 0;
for (var k = 0; k < 20; k++) t += run(3000);
t;
"#;
    let expected = {
        let mut t: i64 = 0;
        for _ in 0..20 {
            let mut s: i32 = 0;
            for i in 0..3000 {
                s += (i & 7) + 1;
                s ^= i;
                s += 2;
            }
            t += s as i64;
        }
        t.to_string()
    };
    assert_eq!(value(src), expected);
    let (v, stencils) = compiled(src, "run");
    assert_eq!(v, expected);
    // The one ToBoolean test left is `peek() ? 1 : 0`'s, after the loop.
    let tests = stencils.iter().filter(|s| s.starts_with("jto")).count();
    assert_eq!(tests, 1, "{stencils:?}");
    // Nor does it load `flag` (in memory, as captured) for them, nor
    // compile the arm never taken.
    assert!(!stencils.iter().any(|s| s == "ldar_m"), "{stencils:?}");
    assert!(!stencils.iter().any(|s| s.starts_with("mul")), "{stencils:?}");
}

/// `x = b || k` and `x = b && k` on a captured `b` known there: the folded
/// test's jump passes `b` on in `acc`, so the load it read stays (#64).
#[test]
fn folded_captured_tests_keep_the_value_they_pass_on() {
    let src = r#"
function run(n) {
  var b = false, s = 0, x;
  var peek = function () { return b; };
  for (var i = 0; i < n; i++) {
    b = 3;
    s = (s + i) | 0;
    x = b || 5;
    s = (s + x) | 0;
    b = 0;
    s = (s ^ i) | 0;
    x = b && 7;
    s = (s + x + 1) | 0;
  }
  return s + (peek() ? 1 : 0);
}
var t = 0;
for (var k = 0; k < 20; k++) t += run(3000);
t;
"#;
    let expected = {
        let mut t: i64 = 0;
        for _ in 0..20 {
            let mut s: i32 = 0;
            for i in 0..3000 {
                s = s.wrapping_add(i).wrapping_add(3);
                s = (s ^ i).wrapping_add(1);
            }
            t += s as i64;
        }
        t.to_string()
    };
    assert_eq!(value(src), expected);
    let (v, stencils) = compiled(src, "run");
    assert_eq!(v, expected);
    // Both tests folded (the one left is `peek() ? 1 : 0`'s).
    let tests = stencils.iter().filter(|s| s.starts_with("jto")).count();
    assert_eq!(tests, 1, "{stencils:?}");
}

/// Polymorphic calls on a local receiver (copied into the call's `this`):
/// `get` is one function for `A` and `B`, another for `C`, and `D` comes
/// after `run` is compiled. The expected value is Node's.
const RECEIVER_DISPATCH: &str = r#"
function Base(v) { this.v = v; }
Base.prototype.get = function () { return this.v; };
function A(v) { this.v = v; }
A.prototype = new Base(0);
function B(v) { this.w = 1; this.v = v; }
B.prototype = A.prototype;
function C(v) { this.v = v * 2; }
C.prototype.get = function () { return this.v + 1; };
function D(v) { this.d = 0; this.v = v; }
D.prototype.get = function () { return 0 - this.v; };
function run(objs, n) {
  var s = 0;
  for (var r = 0; r < n; r++)
    for (var i = 0; i < objs.length; i++) {
      var o = objs[i];
      s += o.get() + o.get();
    }
  return s;
}
function main() {
  var objs = [];
  for (var i = 0; i < 30; i++) objs.push(i % 3 == 0 ? new A(i) : i % 3 == 1 ? new B(i) : new C(i));
  var s = run(objs, 300);
  objs.push(new D(5));
  return s + run(objs, 300);
}
"#;

#[test]
fn calls_dispatched_on_the_receivers_shape_agree_with_node() {
    assert_eq!(value(&format!("{RECEIVER_DISPATCH}\nmain();")), "717000");
}

/// Direct calls from versions of one caller compiled for different types
/// (`many` on `P`, `Q` and a literal): each version's call site is its own,
/// bound to a version of `weight` for its types. And `new Array()` made
/// in compiled code, until `Array` is replaced. The expected value is
/// Node's.
const DIRECT_SITES: &str = r#"
function P(x) { this.x = x; }
function Q(x) { this.y = 0; this.x = x; }
function weight(o, k) { return o.x * k; }
function many(o, n) {
  var s = 0;
  for (var i = 0; i < n; i++) s += weight(o, i & 3) + weight(o, 1);
  return s;
}
function bag() { var a = new Array(); a.push(1); return a.length; }
function bags(n) { var s = 0; for (var i = 0; i < n; i++) s += bag(); return s; }
function main() {
  var s = 0;
  for (var r = 0; r < 200; r++) s += many(new P(r), 50) + many(new Q(r), 50) + many({ x: r }, 5);
  s += bags(3000);
  Array = function () { this.push = function (v) {}; this.length = 7; };
  return s + bags(10);
}
"#;

#[test]
fn direct_calls_from_several_versions_agree_with_node() {
    assert_eq!(value(&format!("{DIRECT_SITES}\nmain();")), "5117370");
}

/// A call site seeing three methods too long to inline: a direct call to
/// each, dispatched on the callee, and `D`'s, which comes after `run` is
/// compiled, through the runtime. The expected value is Node's.
const DIRECT_DISPATCH: &str = r#"
function A(v) { this.v = v; }
A.prototype.f = function (s) {
  s = s + this.v * 1 + 0; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 1; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 2; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 3; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 4; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 5; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 6; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 7; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 8; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 9; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 10; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 11; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 12; if (s > 100000) s = s - 99991;
  s = s + this.v * 1 + 13; if (s > 100000) s = s - 99991;
  return s;
};
function B(v) { this.v = v; }
B.prototype.f = function (s) {
  s = s + this.v * 2 + 0; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 1; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 2; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 3; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 4; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 5; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 6; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 7; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 8; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 9; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 10; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 11; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 12; if (s > 100000) s = s - 99991;
  s = s + this.v * 2 + 13; if (s > 100000) s = s - 99991;
  return s;
};
function C(v) { this.v = v; }
C.prototype.f = function (s) {
  s = s + this.v * 3 + 0; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 1; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 2; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 3; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 4; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 5; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 6; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 7; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 8; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 9; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 10; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 11; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 12; if (s > 100000) s = s - 99991;
  s = s + this.v * 3 + 13; if (s > 100000) s = s - 99991;
  return s;
};
function D(v) { this.v = v; }
D.prototype.f = function (s) {
  s = s + this.v * 4 + 0; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 1; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 2; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 3; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 4; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 5; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 6; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 7; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 8; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 9; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 10; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 11; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 12; if (s > 100000) s = s - 99991;
  s = s + this.v * 4 + 13; if (s > 100000) s = s - 99991;
  return s;
};
function run(objs, n) {
  var s = 0;
  for (var r = 0; r < n; r++)
    for (var i = 0; i < objs.length; i++) s = objs[i].f(s);
  return s;
}
function main() {
  var objs = [];
  for (var i = 0; i < 30; i++) objs.push(i % 3 == 0 ? new A(i) : i % 3 == 1 ? new B(i) : new C(i));
  var s = run(objs, 200);
  objs.push(new D(7));
  return s + run(objs, 200);
}
"#;

#[test]
fn direct_calls_dispatched_on_the_callee_agree_with_node() {
    assert_eq!(value(&format!("{DIRECT_DISPATCH}\nmain();")), "73149");
}

/// A call dispatched on callees that are closures made anew time and
/// again (with upvalues of their own, one with a handler, which runs
/// interpreted): each case is for any closure of its code, and calls it
/// with its own upvalues; then another function and a native join them.
/// The expected value is Node's.
const DISPATCH_BY_CODE: &str = r#"
function make(k) {
  var big = []; for (var j = 0; j < 50; j++) big.push(k + j);
  function a(x) { var s = 0; for (var j = 0; j < 3; j++) s += big[j]; return s + x; }
  function b(x) { try { return big[k % 50] - x; } finally { k++; } }
  function c(x) { return x * 2; }
  return [a, b, c];
}
function run(fs, n) {
  var s = 0;
  for (var i = 0; i < n; i++) { var f = fs[i % fs.length]; s += f(i); }
  return s;
}
function main() {
  var s = 0;
  for (var p = 0; p < 40; p++) s += run(make(p), 3000);
  var fs = make(7);
  fs.push(function (x) { return -x; }, Math.abs);
  return s + run(fs, 3000);
}
"#;

#[test]
fn calls_dispatched_on_the_callees_code_agree_with_node() {
    assert_eq!(value(&format!("{DISPATCH_BY_CODE}\nmain();")), "126052700");
}

/// `typeof` of every kind of value, of an undeclared global (no
/// ReferenceError), and in a compiled loop. The expected value is Node's.
const TYPEOF: &str = r#"
function F() {}
var o = { a: 1 }, arr = [1], s = "x", n = 1.5, i = 3, u, nl = null, t = true;
function kinds() {
  return typeof o + "," + typeof arr + "," + typeof s + "," + typeof n + "," + typeof i + "," + typeof u + "," + typeof nl + "," +
          typeof t + "," + typeof F + "," + typeof Object + "," + typeof notDeclared + "," + typeof (1 + 1) + "," + typeof typeof 1;
}
function hot(v) { var c = 0; for (var k = 0; k < 5000; k++) if (typeof v == "number") c++; return c; }
var r = kinds();
r == "object,object,string,number,number,undefined,object,boolean,function,function,undefined,number,string" ? hot(4) + hot("a") + (typeof window != "undefined" ? 1 : 0) : r;
"#;

#[test]
fn typeof_agrees_with_node() {
    assert_eq!(value(TYPEOF), "5000");
}

/// `try`/`catch`: exceptions thrown two frames down, by the runtime (a
/// TypeError, caught as its message for now), rethrown from a `catch`,
/// a catch parameter shadowing a local, and a throw after a loop compiled
/// code ran. The expected value is Node's.
const TRY_CATCH: &str = r#"
function thrower(n) { if (n % 7 == 3) throw "t" + n; return n; }
function deep(n) { return thrower(n) + 1; }
function bad(o) { return o.x.y; }
function hotCallee(n) { var s = 0; for (var i = 0; i < 2000; i++) s += i; if (n == 42) throw 99; return s; }
function main() {
  var caught = 0, sum = 0, last = "";
  for (var i = 0; i < 3000; i++) {
    try { sum += deep(i); } catch (e) { caught++; last = e; }
  }
  try { bad({}); } catch (e) { last = last + "|" + (typeof e); }
  try { try { throw 1; } catch (a) { throw a + 1; } } catch (b) { sum += b; }
  var e = 5;
  try { throw 10; } catch (e) { sum += e; }
  sum += e;
  for (var k = 40; k < 45; k++) { try { sum += hotCallee(k); } catch (x) { sum += x; } }
  return sum + caught * 1000000 + last.length;
}
"#;

#[test]
fn try_catch_agrees_with_node() {
    assert_eq!(value(&format!("{TRY_CATCH}\nmain();")), "440853270");
}

/// Closures over enclosing functions' variables (upvalues, open while
/// the function runs, closed when it returns): shared by closures of one
/// call, read after the call returned, through an intermediate function,
/// changed by a call within an expression, per recursive call, an IIFE's
/// state, `++` and `+=` on them, and one left open by a throw. The
/// expected value is Node's.
const CLOSURES: &str = r#"
function counter(start) {
  var n = start;
  return { inc: function () { return ++n; }, add: function (k) { n += k; return n; }, get: function () { return n; } };
}
function adders() {
  var fs = [], base = 10;
  for (var i = 0; i < 3; i++) fs.push(function (x) { return x + base; });
  base = 100;
  return fs[0](1) + fs[2](2);
}
function outer() {
  var a = 1;
  function middle() {
    var b = 2;
    function inner() { a = a * 10; return a + b; }
    return inner;
  }
  var f = middle();
  var r = f() + f();
  return r + a;
}
function sideEffect() {
  var x = 1;
  function bump() { x = x + 5; return 0; }
  return x + bump() + x;
}
function rec(n) {
  var me = n;
  var get = function () { return me; };
  if (n == 0) return get();
  return get() + rec(n - 1) + get();
}
var rnd = (function () {
  var seed = 49734321;
  return function () {
    seed = (seed * 31 + 7) % 1000003;
    return seed;
  };
})();
function thrower() {
  var hidden = 7;
  var peek = function () { return hidden; };
  thrower.peek = peek;
  hidden = 8;
  throw "x";
}
function main() {
  var c = counter(5);
  c.inc(); c.add(10); c.inc();
  var t = typeof c.get;
  var r = 0;
  for (var k = 0; k < 2000; k++) r = (r + rnd()) | 0;
  try { thrower(); } catch (e) {}
  return c.get() + adders() + outer() + sideEffect() + rec(4) + r + thrower.peek() + t.length;
}
"#;

#[test]
fn closures_agree_with_node() {
    assert_eq!(value(&format!("{CLOSURES}\nmain();")), "1032368592");
}

/// `%`, `&`, `|`, `^`, `<<`, `>>`, `>>>` on any operands (int32s, doubles
/// past int32, strings, NaN) and a seeded generator like Octane's. The
/// expected value is Node's.
const BITWISE: &str = r#"
function f(a, b) { return (a & b) + (a | b) * 3 + (a ^ b) * 7 + (a << (b & 7)) + (a >> 3) + (a >>> 1) + a % (b | 1); }
var vals = [0, 1, -1, 7, 0x7fffffff, -2147483648, 4294967295, 0xc761c23c, 1.5, -7.25, "12", NaN];
var s = 0;
for (var i = 0; i < vals.length; i++) for (var j = 0; j < vals.length; j++) { var r = f(vals[i], vals[j]); if (r == r) s = (s + r) % 1000000007; }
var seed = 49734321;
for (var k = 0; k < 100; k++) { seed = ((seed + 0x7ed55d16) + (seed << 12)) & 0xffffffff; seed = ((seed ^ 0xc761c23c) ^ (seed >>> 19)) & 0xffffffff; }
s + seed;
"#;

#[test]
fn bitwise_operators_agree_with_node() {
    assert_eq!(value(BITWISE), "346943374");
}

/// Builtins Octane's harness uses: `toFixed`, `toPrecision`, `Math`,
/// `indexOf`, `Date` (but its time), unary `-`, `~`, `+` and postfix
/// increments' values. The expected value is Node's.
const BUILTINS: &str = r#"
var out = "";
var xs = [0, 1, -1, 2.5, -2.5, 0.5, 1.005, 123.456, 1e21, -0.0001234, 98765.4321, NaN, Infinity];
for (var i = 0; i < xs.length; i++) {
  var x = xs[i];
  out = out + x.toFixed(0) + "," + x.toFixed(2) + "," + x.toPrecision(3) + "," + x.toString() + ";";
}
out = out + Math.round(2.5) + Math.round(-2.5) + Math.round(-0.4) + Math.floor(-1.5) + Math.ceil(1.2);
out = out + Math.pow(2, 10) + Math.pow(1, NaN) + Math.log(Math.E) + Math.max(1, 7, 3) + Math.min(4, -2) + Math.abs(-3) + Math.sqrt(16);
var a = [3, "x", 5, 3, null];
out = out + a.indexOf(3) + a.indexOf(3, 1) + a.indexOf("x") + a.indexOf(9) + a.indexOf(null) + a.indexOf(3, -2);
var d = new Date(1000), e = new Date(4000);
out = out + (e - d) + d.getTime() + (typeof Date.now()) + (Date.now() > 1e12) + (Math.random() < 1);
out = out + (-(3)) + (~5) + (+"12") + (+"") + (-"x");
var k = 5; out = out + (k++) + k + (k--) + k;
out;
"#;

#[test]
fn builtins_agree_with_node() {
    assert_eq!(
        value(BUILTINS),
        "0,0.00,0.00,0;1,1.00,1.00,1;-1,-1.00,-1.00,-1;3,2.50,2.50,2.5;-3,-2.50,-2.50,-2.5;1,0.50,0.500,0.5;1,1.00,1.00,1.005;123,123.46,123,123.456;1e+21,1e+21,1.00e+21,1e+21;-0,-0.00,-0.000123,-0.0001234;98765,98765.43,9.88e+4,98765.4321;NaN,NaN,NaN,NaN;Infinity,Infinity,Infinity,Infinity;3-20-221024NaN17-234031-14330001000numbertruetrue-3-6120NaN5665"
    );
}

/// Powers fdlibm's `pow` gets a bit off (an audio synthesizer's gains and
/// frequencies: BeepBox's output differed from the other engines'; and
/// `10 ** -5` wasn't `0.00001`), from `Math.pow`, `**` and a folded `**`,
/// in a loop that gets compiled. The expected values are the correctly
/// rounded ones, which are V8's.
#[test]
fn powers_round_as_in_the_other_engines() {
    assert_eq!(
        value(
            r#"
function f(x, y) { return Math.pow(x, y); }
function g(x, y) { return x ** y; }
var a, b;
for (var i = 0; i < 3000; i++) { a = f(2, -2.5) + f(2, 3.5) + f(0.5, -0.7272970622351479); b = g(2, -0.5); }
[a, b === Math.SQRT1_2, 2 ** -0.5 === Math.SQRT1_2, 2 ** -1.5, g(2, 0.8333333333333334), f(10, -5), f(1, NaN), g(-1, Infinity)].join();"#
        ),
        "13.146019678892063,true,true,0.3535533905932738,1.7817974362806785,0.00001,NaN,NaN"
    );
}

/// A method a compiled loop calls inlined, without checking the callee
/// (its prototype holds it), replaced from a called function and from the
/// loop itself. The expected value is Node's.
const METHODS: &str = r#"
function A(v) { this.v = v; }
A.prototype.f = function (k) { return this.v + k; };
function replace() { A.prototype.f = function (k) { return this.v * k; }; }
function methods() {
  var a = new A(3), s = 0, i;
  for (i = 0; i < 5000; i++) {
    s = (s + a.f(i)) | 0;
    if (i == 2000) replace();
    if (i == 4000) A.prototype.f = function (k) { return k - this.v; };
  }
  return s;
}
"#;

#[test]
fn replaced_prototype_methods_agree_with_node() {
    assert_eq!(value(&format!("{METHODS}\nmethods();")), "24502506");
}

/// Concatenations of strings and int32s around the word boundaries their
/// units are copied in (`interp::concat_fast`), on either side, compiled.
/// The expected value is Node's.
const CONCATS: &str = r#"function concats() {
  var pieces = ["", "a", "bc", "def", "ghij", "klmno", "pqrstuvw", "xyz0123456789ABCDEFG"];
  var ints = [0, 7, -7, 42, 1234, -98765, 2147483647, -2147483648];
  var out = "", n = 0;
  for (var r = 0; r < 300; r++) {
    for (var i = 0; i < pieces.length; i++) {
      for (var j = 0; j < ints.length; j++) {
        var s = pieces[i] + ints[j];
        var t = ints[j] + pieces[i];
        var u = pieces[i] + pieces[(i + j) % pieces.length];
        n += s.length + t.length + u.length;
        if (r == 299 && (i + j) % 5 == 0) out = out + s + "|" + t + "|" + u + ";";
      }
    }
  }
  return n + ":" + out;
}
"#;

#[test]
fn concatenations_agree_with_node() {
    assert_eq!(
        value(&format!("{CONCATS}\nconcats();")),
        "590400:0|0|;-98765|-98765|klmno;a1234|1234a|aklmno;bc42|42bc|bcklmno;def-7|-7def|defklmno;def-2147483648|-2147483648def|defbc;ghij7|7ghij|ghijklmno;ghij2147483647|2147483647ghij|ghijbc;klmno0|0klmno|klmnoklmno;klmno-98765|-98765klmno|klmnobc;pqrstuvw1234|1234pqrstuvw|pqrstuvwbc;xyz0123456789ABCDEFG42|42xyz0123456789ABCDEFG|xyz0123456789ABCDEFGbc;"
    );
}

/// Values compiled code takes as constants (constant fields, see
/// `FieldCell`: a global, a plain object's and a function's fields) changed
/// by a callee, by compiled code itself and through a field holding an
/// object; the results are node's.
const CHANGED_CONSTANTS: &str = r#"
var K = {a: 1, b: 2};
var G = 10;
function F() {}
F.s = 5;
function setG(v) { G = v; }
function setKa(v) { K.a = v; }
function setFs(v) { F.s = v; }
function hot(n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + G + K.a + F.s;
    if (i == 5000) setG(20);
    if (i == 7000) setKa(3);
    if (i == 9000) setFs(7);
  }
  return t;
}
var r = 0;
for (var j = 0; j < 30; j++) r = r + hot(10000);
// direct stores from compiled code
function hot2(n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + K.b;
    if (i == 500) K.b = 9;
  }
  return t;
}
for (var j = 0; j < 30; j++) r = r + hot2(1000);
// global reassigned inside the hot loop itself
var H = 1;
function hot3(n) { var t = 0; for (var i = 0; i < n; i++) { t = t + H; if (i == 100) H = 2; } return t; }
for (var j = 0; j < 30; j++) { H = 1; r = r + hot3(1000); }
// object constants as receivers
var O = {p: {q: 4}};
function hot4(n) { var t = 0; for (var i = 0; i < n; i++) { t = t + O.p.q; if (i == 300) O.p = {q: 6}; } return t; }
for (var j = 0; j < 30; j++) r = r + hot4(1000);
"#;

#[test]
fn changed_constants_agree_with_node() {
    assert_eq!(
        value(&format!(
            "{CHANGED_CONSTANTS}
r;"
        )),
        "9420847"
    );
}

/// More of them: a keyed store, stores in a callee (inlined or not, which
/// the code notices after the call) and `Object.defineProperty`.
const CHANGED_CONSTANTS_2: &str = r#"
var K = {a: 1, b: 2, c: 3, d: 4};
var r = 0;
function hot5(n, key) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + K.a;
    if (i == 400) K[key] = 5;
  }
  return t;
}
for (var j = 0; j < 20; j++) { K.a = 1; r = r + hot5(1000, "a"); }
function setc(v) { K.c = v; }
function hot6(n) { var t = 0; for (var i = 0; i < n; i++) { t = t + K.c; if (i == 600) setc(t % 7); } return t; }
for (var j = 0; j < 20; j++) r = r + hot6(1000);
function big(v) { var x = 0; for (var q = 0; q < 3; q++) { x = x + q; } K.d = v + x; }
function hot7(n) { var t = 0; for (var i = 0; i < n; i++) { t = t + K.d; if (i == 700) big(i); } return t; }
for (var j = 0; j < 20; j++) r = r + hot7(1000);
var Z = {z: 1};
function hot8(n) { var t = 0; for (var i = 0; i < n; i++) { t = t + Z.z; if (i == 800) Object.defineProperty(Z, "z", {value: 3, writable: true}); } return t; }
for (var j = 0; j < 5; j++) r = r + hot8(1000);
"#;

#[test]
fn more_changed_constants_agree_with_node() {
    assert_eq!(
        value(&format!(
            "{CHANGED_CONSTANTS_2}
r;"
        )),
        "13721319"
    );
}

/// Methods shadowed by own properties while compiled code calls them in a
/// loop: added by a callee, after the object became another's prototype,
/// after `Object.defineProperty`, and by a keyed store. The shapes involved
/// were stable (see `Shape::is_stable`) until then. Tiering up after one
/// back edge compiled the loop once the object had the shadowing method,
/// and the method's load took it while the inlined call was the old
/// method's. The results are node's.
const SHADOWED_METHODS: &str = r#"
function P(x) { this.x = x; }
P.prototype.get = function () { return this.x; };
function shadow(o) { o.get = function () { return 100; }; }
var r = 0;
function hot(o, n, k) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + o.get();
    if (i == k) shadow(o);
  }
  return t;
}
for (var j = 0; j < 20; j++) { var o = new P(j); r = r + hot(o, 1000, 500 + j); }
function Q(x) { this.x = x; }
Q.prototype.get = function () { return this.x * 2; };
function protoize(o) { function F() {} F.prototype = o; return new F(); }
function hot2(o, n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + o.get();
    if (i == 300) { var c = protoize(o); o.get = function () { return 7; }; t = t + c.get(); }
  }
  return t;
}
for (var j = 0; j < 20; j++) r = r + hot2(new Q(j), 1000);
function R(x) { this.x = x; }
R.prototype.get = function () { return this.x + 1; };
function hot3(o, n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + o.get();
    if (i == 400) { Object.defineProperty(o, "x", {value: 50, writable: false}); o.get = function () { return this.x; }; }
  }
  return t;
}
for (var j = 0; j < 20; j++) r = r + hot3(new R(j), 1000);
function S(x) { this.x = x; }
S.prototype.get = function () { return this.x; };
function hot4(o, n, key) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + o.get();
    if (i == 600) o[key] = function () { return 3; };
  }
  return t;
}
for (var j = 0; j < 20; j++) r = r + hot4(new S(j), 1000, "get");
"#;

#[test]
fn shadowed_methods_agree_with_node() {
    assert_eq!(
        value(&format!(
            "{SHADOWED_METHODS}
r;"
        )),
        "2110380"
    );
}

/// Loads of constant fields moved out of a loop inlined into its caller
/// (`hoist_loads`), the fields then changed: by a callee, on another
/// object, with the receiver gaining properties first; and a loop not run
/// with `undefined` for its receiver. The results are node's.
const HOISTED_LOADS: &str = r#"
function Box(v) { this.v = v; }
function Holder(b) { this.b = b; }
var G = new Holder(new Box(3));
function setB(h) { h.b = new Box(7); }
function setG(h) { G.b = new Box(11); }
function grow(h) { h.extra = 1; h.more = 2; h.b = new Box(5); }
function noop(h) {}
Holder.prototype.sum = function (n, k, f) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    t = t + this.b.v + G.b.v;
    if (i == k) f(this);
  }
  return t;
};
function scan(o, n) { var t = 0; for (var i = 0; i < n; i++) t = t + o.b.v; return t; }
function drive(h, n, k, f) { return h.sum(n, k, f); }
function drive2(o, n) { return scan(o, n); }
var r = 0;
var fs = [noop, setB, setG, grow];
for (var j = 0; j < 4000; j++) {
  var h = new Holder(new Box(j % 13));
  var f = j % 97 == 5 ? fs[1 + (j % 3)] : noop;
  r = r + drive(h, 20, j % 7, f);
  r = r + drive2(j % 501 == 9 ? undefined : h, j % 501 == 9 ? 0 : 5);
}
"#;

#[test]
fn hoisted_loads_agree_with_node() {
    assert_eq!(
        value(&format!(
            "{HOISTED_LOADS}
r;"
        )),
        "1447227"
    );
}

/// Loads moved out of nested loops of one function (`hoist_loads`), which
/// code tiering up may enter at either loop's header: the field changed
/// in the inner loop by a callee, an inner loop not run, a `while` and a
/// `for` inside the same loop. The results are node's.
const HOISTED_FROM_NESTED_LOOPS: &str = r#"
function B(v) { this.v = v; }
function H(b) { this.b = b; }
function W(h) { this.h = h; }
function swap(h) { h.b = new B(9); }
function f(w, n, m, k) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    for (var j = 0; j < m; j++) {
      t = t + w.h.b.v;
      if (i == k && j == 1) swap(w.h);
    }
    t = t + w.h.b.v * 2;
  }
  return t;
}
function g(w, n, m) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    var j = 0;
    while (j < m) { t = t + w.h.b.v; j++; }
    for (var q = 0; q < 3; q++) { t = t + w.h.b.v; }
  }
  return t;
}
var r = 0;
for (var z = 0; z < 40; z++) {
  r = r + f(new W(new H(new B(z))), 30, 20, z % 5 == 0 ? 7 : 1000);
  r = r + g(new W(new H(new B(z))), 20, z % 3);
}
"#;

#[test]
fn loads_hoisted_from_nested_loops_agree_with_node() {
    assert_eq!(
        value(&format!(
            "{HOISTED_FROM_NESTED_LOOPS}
r;"
        )),
        "542668"
    );
}

/// `this` in sloppy mode (cloison has no strict mode): the global object in a
/// call without a receiver, and for `f.call(null)` / `f.call()`; through
/// inlined, direct, polymorphic and self tail calls. The expected value is
/// Node's.
const SLOPPY_THIS: &str = r#"
var tag = 1;
function small() { return this.tag; }
function big(k) {
  var s = 0;
  for (var j = 0; j < 3; j++) s += this.tag + k + j;
  if (k < 0) { s += 1; s += 2; s += 3; s += 4; s += 5; s += 6; s += 7; s += 8; }
  return s;
}
function other() { return this === undefined ? -1 : 2; }
function count(n, acc) { if (n == 0) return acc + this.tag; return count(n - 1, acc + 1); }
var o = { tag: 5, m: small };
function sloppyThis() {
  var s = 0, fs = [small, other];
  for (var i = 0; i < 3000; i++) {
    s += small();
    s += big(i & 3);
    var f = fs[i & 1];
    s += f();
    s += small.call(null) + small.call() + small.call(o) + small.call(undefined);
    s += o.m();
    if (i % 100 == 0) { s += count(10, 0); tag = (tag + 1) & 7; }
  }
  return s;
}
"#;

#[test]
fn sloppy_this_agrees_with_node() {
    assert_eq!(value(&format!("{SLOPPY_THIS}\nsloppyThis();")), "139107");
}

/// Error objects: constructors of each kind, `name`, `message` and
/// `toString` (through `String()` and `+`), and the runtime's own errors
/// caught as objects of their kind. The expected value is Node's.
const ERRORS: &str = r#"
var e = new Error("boom");
var r = [e.message, e.name, String(e), "" + e, new RangeError().toString(), Error("x").message];
try { null.x; } catch (t) { r.push(t.name, t.message, String(t)); }
try { throw new TypeError("bad"); } catch (t) { r.push(t.message, t.name); }
try { undefinedVar; } catch (t) { r.push(t.name); }
"#;

#[test]
fn error_objects_agree_with_node() {
    assert_eq!(
        value(&format!("{ERRORS}\nr.join(\"|\");")),
        "boom|Error|Error: boom|Error: boom|RangeError|x|TypeError|Cannot read properties of null (reading 'x')|TypeError: Cannot read properties of null (reading 'x')|bad|TypeError|ReferenceError"
    );
}

/// `arguments` (extra arguments included) and `Function.prototype.apply`,
/// from interpreted and compiled callers: forwarded with `apply(this,
/// arguments)` (raytrace's `Class.create`), through `call`, on natives.
/// The expected value is Node's.
const ARGUMENTS: &str = r#"
function sum() { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s; }
function pair(a, b) { return a * 10 + b + (this.k || 0); }
function fwd() { return pair.apply(this, arguments); }
function viaCall(x) { return sum.call(null, x, arguments.length, 3); }
var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var P = Class.create();
P.prototype = { initialize: function (x, y) { this.x = x; this.y = y; } };
function argsAndApply() {
  var t = 0, o = { k: 100, f: fwd };
  for (var i = 0; i < 4000; i++) {
    t += sum(i, 1, 2, 3) + sum() + fwd(i & 7, 2) + o.f(1, 2) + viaCall(i);
    t += pair.apply(o, [i & 3, 1]) + pair.apply(null, [1, 2]) + Math.max.apply(Math, [i, 5, 9]);
    var p = new P(i, 3);
    t += p.x - p.y;
  }
  return t;
}
"#;

#[test]
fn arguments_and_apply_agree_with_node() {
    assert_eq!(value(&format!("{ARGUMENTS}\nargsAndApply();")), "33128045");
}

/// `instanceof` and `in` in compiled code (through the runtime), over
/// prototype chains, arrays and elements, and the errors they throw (the
/// compiled code leaves for the interpreter to throw them). The expected
/// value is Node's.
const INSTANCE_AND_IN: &str = r#"
function A() { this.a = 1; }
function B() { this.b = 2; }
B.prototype = new A();
B.prototype.p = 3;
function check(x, C, key) { return (x instanceof C ? 1 : 0) + (key in x ? 2 : 0); }
function bad(x, C) { return x instanceof C; }
function badIn(k, o) { return k in o; }
function errors(o, i) {
  var t = 0;
  try { bad(o, i & 1 ? 3 : {}); } catch (e) { t += e instanceof TypeError ? 1000 : 0; }
  try { badIn("a", i & 1 ? "str" : 4); } catch (e) { t += e instanceof TypeError ? 3000 : 0; }
  return t;
}
function instanceAndIn() {
  var t = 0, objs = [new A(), new B(), [1, 2], { a: 5 }], keys = ["a", "b", "p", "0", 1, "length"];
  for (var i = 0; i < 4000; i++) {
    var o = objs[i & 3];
    t += check(o, A, keys[i % 6]) + check(o, B, keys[(i + 1) % 6]) * 4 + check(o, Array, keys[(i + 2) % 6]) * 16;
    if (o instanceof B && "p" in o) t += 7;
    if (i % 500 == 0) t += errors(o, i);
  }
  return t;
}
"#;

#[test]
fn instanceof_and_in_agree_with_node() {
    assert_eq!(value(&format!("{INSTANCE_AND_IN}\ninstanceAndIn();")), "108338");
}

/// `finally`: on normal completion, exceptions (thrown again after it),
/// `return` (also one it overrides), `break` and `continue` (labeled, out
/// of nested `try`s), with `catch` clauses inside and around it. The
/// expected value is Node's.
const FINALLY: &str = r#"
var log = "";
function a(x) {
  try {
    try { if (x == 1) return "r1"; if (x == 2) throw "t2"; log += "b"; }
    catch (e) { log += "c" + e; if (x == 2) return "rc"; }
    finally { log += "F"; }
    log += "m";
    for (var i = 0; i < 3; i++) {
      try { if (i == 1) continue; if (i == 2) break; log += i; } finally { log += "f" + i; }
    }
    return "end";
  } finally { log += "O"; }
}
function b() { try { return "x"; } finally { return "override"; } }
function c() { try { throw new Error("in"); } finally { log += "cf"; } }
function d() {
  var n = 0;
  outer: for (var i = 0; i < 3; i++) {
    try { for (var j = 0; j < 3; j++) { try { if (j == 1) continue outer; n++; } finally { n += 10; } } }
    finally { n += 100; }
  }
  return n;
}
function e2() { try { try { throw 1; } finally { log += "e"; } } catch (x) { return "caught" + x; } }
function f2() { try { return 1; } finally { try { throw 2; } catch (q) { log += "q" + q; } } }
var r = [a(0), a(1), a(2), b(), d(), e2(), f2()];
try { c(); } catch (err) { r.push(err.message); }
var s = 0;
for (var k = 0; k < 3000; k++) { try { s += k; if (k % 1000 == 0) throw k; } catch (x) { s -= 1; } finally { s += 2; } }
r.push(s, log);
"#;

#[test]
fn finally_agrees_with_node() {
    assert_eq!(
        value(&format!("{FINALLY}\nr.join(\",\");")),
        "end,r1,rc,override,363,caught1,1,in,4504497,bFm0f0f1f2OFOct2FOeq2cf"
    );
}

/// Accessor properties: from `Object.defineProperty` (on a prototype,
/// getter and setter defined apart: box2d's `b2ContactID.key`) and object
/// literals, from compiled loops (the runtime calls them), nested, one
/// throwing; and an array's `length` set. The expected value is Node's.
const ACCESSORS: &str = r#"
function L() { this._key = 0; this.f = { r: 0 }; }
Object.defineProperty(L.prototype, "key", { enumerable: false, configurable: true,
  get: function () { return this._key; } });
Object.defineProperty(L.prototype, "key", { enumerable: false, configurable: true,
  set: function (b) { this._key = b; this.f.r = b & 255; } });
var box = { _v: 2, get v() { return this._v * 3; }, set v(x) { this._v = x - 1; },
  get nested() { return this.v + 1; } };
function Bad() {}
Object.defineProperty(Bad.prototype, "boom", { get: function () { throw new RangeError("no"); } });
function accessors() {
  var t = 0, l = new L(), arr = [1, 2, 3, 4, 5];
  for (var i = 0; i < 3000; i++) {
    l.key = i;
    t += l.key + l.f.r;
    box.v = i & 7;
    t += box.v + box.nested;
    if (i % 1000 == 0) {
      try { t += new Bad().boom; } catch (e) { t += e instanceof RangeError ? 100 : 0; }
      arr.length = 2; t += arr.length; arr.length = 4; t += arr.length + (arr[3] === undefined ? 1 : 0);
      arr.push(9); t += arr[4];
    }
  }
  var keys = [];
  for (var k in l) keys.push(k);
  for (var k2 in box) keys.push(k2);
  return t + ":" + keys.join(",") + ":" + Object.keys(box).length + ":" + ("key" in l) + l.hasOwnProperty("key");
}
"#;

#[test]
fn accessors_agree_with_node() {
    assert_eq!(value(&format!("{ACCESSORS}\naccessors();")), "4922724:_key,f,_v,v,nested:3:truefalse");
}

/// Typed arrays and buffers read and written by compiled loops (elements
/// through the runtime), mixed with arrays and strings; callbacks from
/// natives (`sort`, `map`, `replace`), JSON, `eval` and `Function`. The
/// expected value is Node's.
const TYPED_AND_CALLBACKS: &str = r#"
function fill(t, n) { for (var i = 0; i < n; i++) t[i] = i * 37; return t; }
function sum(t) { var s = 0; for (var i = 0; i < t.length; i++) s += t[i]; return s; }
function mixed(xs) { var s = 0; for (var k = 0; k < xs.length; k++) { var x = xs[k]; for (var i = 0; i < 4; i++) s += x[i] | 0; } return s; }
function typedAndCallbacks() {
  var u8 = fill(new Uint8Array(300), 300), i16 = fill(new Int16Array(300), 300);
  var f64 = fill(new Float64Array(300), 300), arr = fill([], 300);
  var heap = new ArrayBuffer(64), h32 = new Int32Array(heap), h8 = new Uint8Array(heap);
  for (var i = 0; i < 16; i++) h32[i] = i * 16843009;
  var r = [sum(u8), sum(i16), sum(f64), sum(arr), sum(h8), mixed([u8, arr, "abcd", h32, f64])];
  var words = [];
  for (var j = 0; j < 200; j++) words.push("w" + ((j * 7919) % 200));
  words.sort(function (a, b) { return a < b ? -1 : a > b ? 1 : 0; });
  r.push(words.slice(0, 3).join("-"), [3, 1, 2].map(function (x) { return x * x; }).join("-"));
  r.push("a1b22c333".replace(/\d+/g, function (d) { return "<" + d.length + ">"; }));
  r.push(JSON.stringify({ k: [1, "x"], n: null }), JSON.parse("[1,{\"a\":[2]}]")[1].a[0]);
  r.push(eval("1 + 2 * 3"), Function("a", "b", "return a * b")(6, 7));
  return r.join(",");
}
"#;

#[test]
fn typed_arrays_and_callbacks_agree_with_node() {
    assert_eq!(
        value(&format!("{TYPED_AND_CALLBACKS}\ntypedAndCallbacks();")),
        "37690,1659450,1659450,1659450,480,101058720,w0-w1-w10,9-1-4,a<1>b<2>c<3>,{\"k\":[1,\"x\"],\"n\":null},2,7,42"
    );
}

/// Functions with more variables than registers: the rest in a scope
/// object, also read by a nested function. The expected value is Node's.
#[test]
fn many_variables_agree_with_node() {
    let src =
        std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/octane/many-variables.js.txt")).unwrap();
    assert_eq!(value(&src), "2001571,4543976");
}

/// A function with more variables than registers, read and written by a
/// function nested in an async function or a generator, which has a scope
/// object of its own: the outer one is passed down under another name. The
/// expected value is V8's and SpiderMonkey's.
#[test]
fn scope_objects_pass_through_async_functions_and_generators() {
    let vars = |p: &str| (0..300).map(|i| format!("var {p}{i} = {i};\n")).collect::<String>();
    let src = format!(
        r#"
var log = [];
(() => {{
{v}
  var counter = 0;
  function bump(x) {{ counter += x; return counter; }}
  var a = async (t) => {{ await 0; var g = function (y) {{ counter = counter + y; v299 += 1; return bump(y); }}; return g(t) + g(t); }};
  var gen = function* () {{ var h = () => {{ v0 += 10; return v0 + v299; }}; yield h(); yield h(); }};
  for (var x of gen()) log.push("gen " + x);
  function inner() {{
{w}
    var z = async function () {{ await null; return [1, 2].map(function (k) {{ return k + w299 + v299 + counter; }}); }};
    return z();
  }}
  var fs = [];
  var b = async () => {{ for (let i = 0; i < 3; i++) {{ await i; fs.push(function () {{ return i + v1 + counter; }}); }} }};
  a(5).then(r => {{ log.push("a " + r + " " + counter + " " + v299);
    return inner(); }}).then(r => {{ log.push("inner " + r);
    return b(); }}).then(() => {{ log.push("fs " + fs.map(f => f()).join()); globalThis.out = log.join(" | "); }});
}})();
"#,
        v = vars("v"),
        w = vars("w")
    );
    let module = cloison::js::compile(&src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("out");
    let g = vm.global_object();
    assert_eq!(display(vm.get(g, a).unwrap()), "gen 309 | gen 319 | a 30 20 301 | inner 621,622 | fs 21,22,23");
}

/// A function with more variables than registers whose loops declare
/// block-scoped variables that closures use: each iteration's is a binding
/// of its own (CreatePerIterationEnvironment, ForIn/OfBodyEvaluation's new
/// environment, a block's each time it is entered), so they keep registers,
/// not the one scope object. (A DOM shim in a bundle's 22,000-line function
/// defined a canvas's `width` and `height` accessors this way: both were
/// `height`'s.) The expected value is V8's.
#[test]
fn loop_bindings_stay_fresh_past_the_registers() {
    let vars: String = (0..300).map(|i| format!("var x{i} = {i};\n")).collect();
    let src = format!(
        r#"(() => {{
{vars}
  var fs = [];
  for (const [k, d] of [["a", 1], ["b", 2]]) fs.push(() => k + d);
  for (const v of [1, 2]) fs.push(() => v);
  for (let i = 0; i < 2; i++) fs.push(() => i);
  for (const key in {{ p: 1, q: 2 }}) fs.push(() => key);
  var n = 0;
  while (n < 2) {{ let w = n++; const c = w * 10; fs.push(() => w + c); }}
  do {{ let w = n++; fs.push(() => w); }} while (n < 4);
  for (var j = 0; j < 2; j++) {{ function g() {{ return j0; }} let j0 = j; fs.push(g); }}
  for (var j = 0; j < 2; j++) {{ try {{ throw j; }} catch (e) {{ fs.push(() => e); }} }}
  for (var j = 0; j < 2; j++) {{ class C {{ static v = j; }} fs.push(() => C.v); }}
  l: for (var j = 0; j < 2; j++) {{ switch (j) {{ default: let s = j; fs.push(() => s); }} }}
  if (n) {{ for (const {{ a, b = a }} of [{{ a: 1 }}, {{ a: 2, b: 3 }}]) fs.push(() => a + b); }}
  return fs.map(f => f()).join() + " " + x299;
}})();"#
    );
    assert_eq!(value(&src), "a1,b2,1,2,0,1,p,q,0,11,2,3,0,1,0,1,0,1,0,1,2,5 299");
}

/// A `finally` block run by a `continue` or `break` leaving its `try` is
/// entered as any block: new bindings, which closures made then keep.
#[test]
fn a_finally_block_left_early_has_new_bindings() {
    let src = r#"
var fs = [];
(() => {
  for (const r of [4, 5, 6]) { try { if (r < 6) continue; } finally { let t = r * 3; fs.push(() => t); } }
  for (const r of [7, 8]) { try { if (r > 7) break; } finally { function g() { return u; } let u = r + 1; fs.push(g); } }
})();
fs.map(f => f()).join();"#;
    assert_eq!(value(src), "12,15,18,8,9");
}

/// A function using `a` variables of one enclosing function and `b` of
/// another (each with no more variables than registers, so none is in a
/// scope object): `a + b` upvalues.
fn upvalues_of_two_functions(a: usize, b: usize) -> String {
    let a_vars: String = (0..a).map(|i| format!("var a{i} = {i};")).collect();
    let b_vars: String = (0..b).map(|i| format!("var b{i} = {};", 1000 + i)).collect();
    let names: Vec<String> = (0..a).map(|i| format!("a{i}")).chain((0..b).map(|i| format!("b{i}"))).collect();
    format!(
        "function outer() {{ {a_vars} function mid() {{ {b_vars} function inner() {{ return [{}]; }} return inner; }} return mid(); }}\n\
         var p = outer()(); [p.length, p[255], p[p.length - 1]].join();",
        names.join(",")
    )
}

/// An upvalue's index is a byte: 256 upvalues compile, more are refused
/// (#136: they read the wrong variables). The expected values are
/// SpiderMonkey's, JSC's and QuickJS's.
#[test]
fn more_than_256_upvalues_are_refused() {
    assert_eq!(value(&upvalues_of_two_functions(128, 128)), "256,1127,1127");
    for (a, b) in [(150, 107), (150, 150)] {
        let e = compile_error(&upvalues_of_two_functions(a, b));
        assert!(e.contains("unsupported: more than 256 upvalues"), "{a} + {b}: {e}");
    }
}

/// Cons strings (long strings built by `+`, flattened when read) and
/// sparse integer keys (an object used as a map by numbers), `push` on an
/// array-like. The expected value is Node's.
const CONS_AND_SPARSE: &str = r#"
function build(n) { var s = ""; for (var i = 0; i < n; i++) s += String.fromCharCode(97 + i % 26); return s; }
function sparseMap() {
  var m = {}, t = 0;
  for (var i = 0; i < 3000; i++) m[i * 100003 % 9999991] = i;
  for (var j = 0; j < 3000; j++) t += m[j * 100003 % 9999991];
  m[5] = 7;
  var keys = 0;
  for (var k in m) keys++;
  return t + ":" + keys + ":" + (2000000 in m) + (100003 in m) + m.hasOwnProperty(100003) + (delete m[100003]) + (100003 in m) + m[5];
}
function consAndSparse() {
  var s = build(5000), parts = [];
  parts.push(s.length, s.charAt(4999), s.indexOf("xyz"), (s + s).length, s.slice(26, 30));
  var a = build(300), b = build(300);
  parts.push(a == b, a === b, (a + "!").length, (a + 1).charAt(300), ("" + 12 + a).substr(0, 5));
  var arr = [s, a].join("|").length;
  parts.push(arr, sparseMap(), JSON.stringify({ a: build(260) }).length);
  var big = [];
  big.push.apply(big, [1, 2]);
  var like = { length: 0 };
  Array.prototype.push.call(like, 5, 6);
  parts.push(like.length, like[1], big.length, "abc".toLocaleUpperCase());
  return parts.join(",");
}
"#;

/// An async function declared in the body of a function with parameter
/// defaults: bound by the body's name for it, as a plain function is
/// (pdf.js's evaluator read its `handleSetFont` as an undeclared global,
/// and extracted no text). The expected value is V8's and SpiderMonkey's.
#[test]
fn async_function_declarations_in_bodies_with_parameter_defaults() {
    let src = r#"
var log = [];
function g(a = 1) { async function h() { return 5; } return (() => h())(); }
class C { async m({ s, seen = new Set() }) { async function hs(x) { return x + 1; } return new Promise(function body(res) { res(hs(s)); }); } }
function w(h3 = 2) { async function h3() { return 9; } return [typeof h3, (() => typeof h3)()].join(); }
async function* ag(a = 1) { async function* q() { yield 7; } for await (const x of (() => q())()) yield x; }
Promise.all([g(), new C().m({ s: 1 })]).then(async v => { for await (const x of ag()) v.push(x); v.push(w()); globalThis.out = v.join(" "); });
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("out");
    let g = vm.global_object();
    assert_eq!(display(vm.get(g, a).unwrap()), "5 2 7 function,function");
}

/// `Intl.Collator` (the `intl` feature): ICU4X's root collation for
/// English, its options (sensitivity, numeric, caseFirst, punctuation),
/// in the options or the locale's `-u-` keywords, and the locale it
/// resolves to. V8's and SpiderMonkey's results.
#[cfg(feature = "intl")]
#[test]
fn intl_collators_compare_by_locale_and_options() {
    let src = r#"
var words = ["b", "a", "B", "A", "file10", "file9", "_x", "$y", "ä", "a1", "a01", "Zeta", "zeta", "éclair", "eclair", "x-y", "xy", "ß", "ss"];
var c = new Intl.Collator(undefined, { usage: "sort", sensitivity: "variant", numeric: true });
var cmp = (o, xs) => xs.map(x => new Intl.Collator("en", o).compare("a", x)).join(",");
var r = [
  words.slice().sort(c.compare).join(" "),
  JSON.stringify(c.resolvedOptions()),
  c.compare === c.compare, c.compare.length, c.compare.name === "",
  Object.prototype.toString.call(c), String(Intl),
  ["b", "a", "B", "A"].sort(new Intl.Collator("en", { caseFirst: "upper" }).compare).join(""),
  ["b", "a", "B", "A"].sort(Intl.Collator("en-u-kf-lower").compare).join(""),
  cmp({ sensitivity: "base" }, ["á", "A"]), cmp({ sensitivity: "accent" }, ["á", "A"]),
  cmp({ sensitivity: "case" }, ["á", "A"]), cmp({}, ["á", "A", "b"]),
  new Intl.Collator("en", { ignorePunctuation: true }).compare("a-b", "ab"),
  new Intl.Collator("en-US-u-kn-kf-upper", { caseFirst: "lower" }).resolvedOptions().locale,
  Intl.Collator.supportedLocalesOf(["EN-us", "en-u-co-phonebk", "en-GB-oxendict"]).join(),
  c.compare(undefined, "undefined"),
];
for (var f of [() => new Intl.Collator("en_US"), () => new Intl.Collator("en", { usage: "x" }), () => Intl.Collator.prototype.compare, () => new Intl.Collator([1])])
  try { f(); r.push("no error") } catch (e) { r.push(e.name) }
class C extends Intl.Collator {}
r.push(new C() instanceof C, new C().compare("a", "b"));
r.join(" | ");
"#;
    assert_eq!(
        value(src),
        "_x $y a A ä a1 a01 b B eclair éclair file9 file10 ss ß x-y xy zeta Zeta | {\"locale\":\"en-US\",\"usage\":\"sort\",\"sensitivity\":\"variant\",\"ignorePunctuation\":false,\"collation\":\"default\",\"numeric\":true,\"caseFirst\":\"false\"} | true | 2 | true | [object Intl.Collator] | [object Intl] | AaBb | aAbB | 0,0 | -1,0 | 0,-1 | -1,-1,-1 | 0 | en-US-u-kn | en-US,en-u-co-phonebk,en-GB-oxendict | 0 | RangeError | RangeError | TypeError | TypeError | true | -1"
    );
}

/// `String.prototype.localeCompare` with the `intl` feature compares as
/// `Intl.Collator(locales, options)` does (ECMA-402), not by code units:
/// TiddlyWiki sorts its titles with it. V8's results.
#[cfg(feature = "intl")]
#[test]
fn locale_compare_collates_as_intl_collator() {
    let src = r#"
var words = ["--tp x", "\"A free\"", "Résumé", "Édition", "Zed", "apple", "Apple", "a b", "ab", "a-b", "$:/x", "_u", "1", "10", "2", "résumé", "Edition", "e"];
var log = [];
var r = [
  words.slice().sort((x, y) => x.localeCompare(y)).join("|"),
  words.slice().sort((x, y) => x.localeCompare(y, undefined, { numeric: true, sensitivity: "base" })).join("|"),
  "a".localeCompare("B"), "a".localeCompare("A"), "a".localeCompare("A", undefined, { sensitivity: "base" }),
  "a".localeCompare("A", "en", { caseFirst: "upper" }), "file10".localeCompare("file9", "en-u-kn"),
  "\u00e9".localeCompare("e\u0301"), "a".localeCompare(), "undefined".localeCompare(), String.prototype.localeCompare.length,
  "a".localeCompare({ toString() { log.push("that"); return "b" } }, undefined, { get usage() { log.push("usage") }, get sensitivity() { log.push("sensitivity") } }),
  log.join(),
];
for (var f of [() => "a".localeCompare("b", "en_US"), () => "a".localeCompare("b", "en", { sensitivity: "x" }), () => String.prototype.localeCompare.call(null, "b")])
  try { f(); r.push("no error") } catch (e) { r.push(e.name) }
// `that` failing to convert throws before the options are read.
log = [];
for (var that of [Symbol(), { toString() { throw new RangeError("that") } }])
  try { "a".localeCompare(that, { get length() { log.push("locales") } }, { get usage() { log.push("usage") } }) } catch (e) { r.push(e.name) }
r.push(log.length);
r.join(" ; ");
"#;
    assert_eq!(
        value(src),
        "_u|--tp x|\"A free\"|$:/x|1|10|2|a b|a-b|ab|apple|Apple|e|Edition|Édition|résumé|Résumé|Zed ; \
         _u|--tp x|\"A free\"|$:/x|1|2|10|a b|a-b|ab|apple|Apple|e|Édition|Edition|Résumé|résumé|Zed ; \
         -1 ; -1 ; 0 ; 1 ; 1 ; 0 ; -1 ; 0 ; 1 ; -1 ; that,usage,sensitivity ; RangeError ; RangeError ; TypeError ; TypeError ; RangeError ; 0"
    );
}

/// `Intl.Segmenter` (the `intl` feature) of graphemes: UAX #29's extended
/// grapheme clusters by ICU4X (CR LF, a combining sequence, an emoji ZWJ
/// sequence and a flag are one segment each), as a Segments object's
/// iterator and its `containing` give them. Univer makes one at startup.
/// V8's results.
#[cfg(feature = "intl")]
#[test]
fn intl_segmenter_segments_graphemes() {
    let src = r#"
var show = s => Array.from(new Intl.Segmenter().segment(s), x => x.segment.length + "@" + x.index).join(",");
var sg = new Intl.Segmenter("en-US-u-co-phonebk", { granularity: "grapheme" });
var ss = sg.segment("àb\n\u{1F600}c"), it = ss[Symbol.iterator](), order = [];
new Intl.Segmenter(undefined, { get localeMatcher() { order.push("localeMatcher") }, get granularity() { order.push("granularity") } });
var r = [
  show("a\r\nb"), show("éx"), show("\u{1F468}‍\u{1F469}‍\u{1F467}!"),
  show("\u{1F1EB}\u{1F1F7}\u{1F1E9}\u{1F1EA}\u{1F1EF}"), show("한각"), show(""), show(12),
  JSON.stringify(sg.resolvedOptions()), Object.prototype.toString.call(sg),
  [-1, 0, 1, 2, 3, 4, 5, 6, 7, "2", NaN, Infinity].map(i => { var d = ss.containing(i); return d ? d.index + ":" + d.segment.length : d }).join(),
  JSON.stringify(sg.segment("ab").containing(1)), ss.containing(0).input === ss.containing(6).input,
  Object.prototype.toString.call(it), it[Symbol.iterator]() === it, JSON.stringify(it.next()),
  [...it].map(x => x.index).join(), JSON.stringify(it.next()),
  Object.getPrototypeOf(ss) === Object.getPrototypeOf(sg.segment("")), Object.getOwnPropertyNames(Object.getPrototypeOf(ss)).join(),
  ss[Symbol.iterator].name, Intl.Segmenter.length, Intl.Segmenter.prototype.segment.length,
  Intl.Segmenter.supportedLocalesOf(["EN-us", "zz", "en-GB"]).join(), order.join(),
];
for (var f of [() => Intl.Segmenter(), () => new Intl.Segmenter("en", { granularity: "line" }), () => new Intl.Segmenter("en", { granularity: "word" }),
               () => new Intl.Segmenter("en", { granularity: "sentence" }), () => new Intl.Segmenter("en", null), () => new Intl.Segmenter("x_y"),
               () => sg.segment.call(new Intl.Collator(), "a"), () => Intl.Collator.prototype.resolvedOptions.call(sg),
               () => ss.containing.call(sg, 0), () => it.next.call(ss), () => ss[Symbol.iterator].call(it), () => ss.containing(Symbol())])
  try { f(); r.push("no error") } catch (e) { r.push(e.name) }
class S extends Intl.Segmenter {}
r.push(new S() instanceof S, new S().segment("ab").containing(1).segment);
r.join(" | ");
"#;
    assert_eq!(
        value(src),
        "1@0,2@1,1@3 | 2@0,1@2 | 8@0,1@8 | 4@0,4@4,2@8 | 1@0,3@1 |  | 1@0,1@1 | \
         {\"locale\":\"en-US\",\"granularity\":\"grapheme\"} | [object Intl.Segmenter] | \
         ,0:2,0:2,2:1,3:1,4:2,4:2,6:1,,2:1,0:2, | {\"segment\":\"b\",\"index\":1,\"input\":\"ab\"} | true | \
         [object Segmenter String Iterator] | true | \
         {\"value\":{\"segment\":\"a\u{300}\",\"index\":0,\"input\":\"a\u{300}b\\n😀c\"},\"done\":false} | \
         2,3,4,6 | {\"done\":true} | true | containing | [Symbol.iterator] | 0 | 1 | en-US,en-GB | \
         localeMatcher,granularity | TypeError | RangeError | no error | no error | TypeError | RangeError | TypeError | \
         TypeError | TypeError | TypeError | TypeError | TypeError | true | b"
    );
}

/// `Intl.Segmenter` of words and sentences: UAX #29's by ICU4X, with
/// `isWordLike` on a word segmenter's segments (`+` here). Node's
/// results, but `a:b` (one word by the default rules, three by CLDR's)
/// and the scripts whose words take a dictionary, which there is none
/// of: a run of Chinese or Japanese, or Thai, is one word. And
/// `containing` finds the iterator's segments, asked in any order (of a
/// string longer than its marks are apart, with words longer too).
#[cfg(feature = "intl")]
#[test]
fn intl_segmenter_segments_words_and_sentences() {
    let src = r#"
var cut = (g, s) => Array.from(new Intl.Segmenter("en", { granularity: g }).segment(s), x => (x.isWordLike ? "+" : "") + x.segment).join("|");
var w = new Intl.Segmenter("en", { granularity: "word" }), st = new Intl.Segmenter(undefined, { granularity: "sentence" });
var r = [
  cut("word", "Hello, world! It's 3.14 e.g. can't"), cut("word", "a:b x_y \u{1F600}\r\n"),
  cut("word", "Hello世界です カタカナ สวัสดีครับ 한국어"),
  cut("sentence", "Hello world. How are you? Mr. Smith went. \"Yes!\" he said.\nNew"), cut("sentence", ""), cut("word", ""),
  JSON.stringify(w.segment("ab cd").containing(3)), JSON.stringify(w.segment(" ").containing(0)),
  JSON.stringify(st.segment("A. B.").containing(4)), JSON.stringify([...w.segment("a ")[Symbol.iterator]()].map(x => Object.keys(x).join())),
  w.resolvedOptions().granularity, st.resolvedOptions().granularity,
];
// `containing` at every index, in any order, is the iterator's segment
// there (in a string longer than its marks are apart).
var s = ""; for (var i = 0; s.length < 6000; i++) s += "word" + i + (i % 7 ? " " : ". ") + (i % 5 ? "" : "é\u{1F600}\n") + (i % 40 ? "" : "x".repeat(600) + " ");
for (var g of ["grapheme", "word", "sentence"]) {
  var segs = new Intl.Segmenter("en", { granularity: g }).segment(s), at = [], bad = 0, seed = 1;
  for (var x of segs) for (var i = 0; i < x.segment.length; i++) at.push(x);
  var ask = i => { var d = segs.containing(i), x = at[i]; if (d.index !== x.index || d.segment !== x.segment || d.isWordLike !== x.isWordLike) bad++ };
  for (var i = s.length - 1; i >= 0; i -= 3) ask(i);
  for (var i = 0; i < 2000; i++) ask((seed = seed * 16807 % 2147483647) % s.length);
  for (var i = 0; i < s.length; i++) ask(i);
  r.push(bad);
}
r.join(" ; ");
"#;
    assert_eq!(
        value(src),
        "+Hello|,| |+world|!| |+It's| |+3.14| |+e.g|.| |+can't ; +a:b| |+x_y| |😀|\r\n ; \
         +Hello|+世界です| |+カタカナ| |+สวัสดีครับ| |+한국어 ; \
         Hello world. |How are you? |Mr. |Smith went. |\"Yes!\" |he said.\n|New ;  ;  ; \
         {\"segment\":\"cd\",\"index\":3,\"input\":\"ab cd\",\"isWordLike\":true} ; \
         {\"segment\":\" \",\"index\":0,\"input\":\" \",\"isWordLike\":false} ; \
         {\"segment\":\"B.\",\"index\":3,\"input\":\"A. B.\"} ; \
         [\"segment,index,input,isWordLike\",\"segment,index,input,isWordLike\"] ; word ; sentence ; 0 ; 0 ; 0"
    );
}

/// `localeCompare` with a locale string and no options, whose collator
/// is kept: the same results call after call (Node's), a `RangeError`
/// for a malformed tag each time, and an array's elements read each time.
#[cfg(feature = "intl")]
#[test]
fn locale_compare_keeps_a_locale_strings_collator() {
    let src = r#"
var n = 0, log = [];
var r = [
  "a".localeCompare("B", "en"), "a".localeCompare("B", "en"), "ä".localeCompare("z", "en"), "a".localeCompare("A", "en-u-kf-upper"), "a".localeCompare("A", "en-u-kf-upper"),
  "file10".localeCompare("file9", "en-u-kn"), "file10".localeCompare("file9", "en"), "file10".localeCompare("file9", "EN-u-kn"),
  "a".localeCompare("B", ["en"]),
];
for (var i = 0; i < 3; i++) for (var tag of ["x_y", "en-", ""]) try { "a".localeCompare("b", tag) } catch (e) { if (e instanceof RangeError) n++ }
r.push(n);
// A String object's or an array's locales are read (observably) each time.
var l = ["en"]; Object.defineProperty(l, 0, { get() { log.push("0"); return "en" } });
"a".localeCompare("b", l); "a".localeCompare("b", l);
r.push(log.join());
r.join(" ; ");
"#;
    assert_eq!(value(src), "-1 ; -1 ; -1 ; 1 ; 1 ; 1 ; -1 ; 1 ; -1 ; 9 ; 0,0");
}

/// Without the `intl` feature, `localeCompare` compares code units.
#[cfg(not(feature = "intl"))]
#[test]
fn locale_compare_is_by_code_units_without_intl() {
    assert_eq!(
        value(r#"["a".localeCompare("B"), "a".localeCompare("a"), "a".localeCompare("b", "en", {})].join()"#),
        "1,0,-1"
    );
}

/// Without the `intl` feature, there is no `Intl`.
#[cfg(not(feature = "intl"))]
#[test]
fn intl_is_absent_without_its_feature() {
    assert_eq!(value("typeof Intl"), "undefined");
}

#[test]
fn cons_strings_and_sparse_keys_agree_with_node() {
    assert_eq!(
        value(&format!("{CONS_AND_SPARSE}\nconsAndSparse();")),
        "5000,h,23,10000,abcd,true,true,301,1,12abc,5301,4498500:3001:falsetruetruetruefalse7,268,2,6,2,ABC"
    );
}

/// What Crypto needed compiled: bitwise operators on registers (int32,
/// doubles, strings, `>>>` past int32), `x++`'s `ToNumber`, a `Mov` with
/// `acc` live (`this.am(0, …, 0, n)`), and keyed stores the stencils gave
/// up on, through the runtime (holes, appends, typed arrays, element
/// transitions). The expected values are Node's.
const SPEED1: &str = r#"
function bitsLoop(n, x) {
  var s = 0, t = 0, u = 0;
  for (var i = 0; i < n; i++) {
    var y = i * 7919;
    s = (s + (y & x) + (y | i) - (y ^ x)) | 0;
    t = (t + (i << (y & 15)) + (y >> (i & 7))) | 0;
    u += (y >>> (i & 3)) + ((-i) >>> 1);
  }
  return s + ":" + t + ":" + u;
}
function bitsMixed() {
  var vals = [5, -1, 3.7, -2.5e10, "12", "x", null, undefined, true, 4294967295, 2147483648, 0x7fffffff];
  var out = "";
  for (var r = 0; r < 100; r++)
    for (var a = 0; a < vals.length; a++)
      for (var b = 0; b < vals.length; b++) {
        var p = vals[a], q = vals[b];
        var v = (p & q) + (p | q) * 3 + (p ^ q) * 5 + (p << q) * 7 + (p >> q) * 11 + (p >>> q) * 13;
        if (r == 99) out += v + ",";
      }
  return out;
}
function am(i, x, w, j, c, n) {
  var xl = x & 0x3fff, xh = x >> 14;
  while (--n >= 0) {
    var l = w[i] & 0x3fff;
    var h = w[i++] >> 14;
    var m = xh * l + h * xl;
    l = xl * l + ((m & 0x3fff) << 14) + w[j] + c;
    c = (l >> 28) + (m >> 14) + xh * h;
    w[j++] = l & 0xfffffff;
  }
  return c;
}
function Big() { this.w = []; for (var q = 0; q < 64; q++) this.w.push((q * 2654435761) & 0xfffffff); }
Big.prototype.am = am;
Big.prototype.reduce = function (x) {
  var c = 0;
  for (var q = 0; q < 40; q++) c = this.am(0, (q * 40503) & 0xfffffff, this.w, 32, 0, 30) + (c & 0xff);
  return c;
};
function postInc() {
  var o = { n: "5" }, s = "7", out = [];
  for (var q = 0; q < 3000; q++) {
    var p = q; p++;
    var z = o.n; z++;
    var e = s, f = e++;
    if (q == 2999) out.push(p, z, typeof f, f, e);
  }
  return out.join(",");
}
function runtimeStores() {
  var holes = [], appended = [], u8 = new Uint8Array(8), f64 = new Float64Array(8), o = {}, sum = 0;
  for (var r = 0; r < 300; r++) {
    var down = [];
    for (var k = 20; k >= 0; k--) down[k] = k * r;
    var up = [];
    for (var k2 = 0; k2 < 20; k2++) up[k2] = r & 1 ? k2 + 0.5 : k2;
    var targets = [holes, u8, f64, o, appended];
    for (var t = 0; t < 5; t++) targets[t][(r + t) & 7] = r * 1.25;
    appended[appended.length] = r;
    sum += down[0] + down[20] + up[19] + holes.length;
  }
  return [sum, holes.join("|"), u8.join("|"), f64.join("|"), o[3], appended.length].join(";");
}
function speed1() {
  var b = new Big(), red = 0;
  for (var k = 0; k < 300; k++) red = (red + b.reduce()) | 0;
  var bl = "";
  for (var k3 = 0; k3 < 300; k3++) bl = bitsLoop(50, k3);
  return [bl, bitsMixed(), red, postInc(), runtimeStores()].join("\n");
}
"#;

#[test]
fn crypto_paths_agree_with_node() {
    assert_eq!(
        value(&format!("{SPEED1}\nspeed1();")),
        r#"6571:7703403:105231322806
1140,-15032385564,332,6158430403,143448,195,195,195,154,-15032385564,-17179868989,2147483620,1744830188,-15032385538,6979321756,52755359705,13602736,55834574809,55834574809,55834574809,27917287374,-15032385538,64424509401,-23622320130,724,-15032385556,180,6158430325,86136,117,117,117,86,-15032385556,-17179869067,2147483628,-1216797528,-3079215112,21512081944,26943132160,10434839488,30022347264,30022347264,30022347264,26173328392,-3079215112,12842478080,14100654072,2776,-56,816,6158430676,344112,468,468,468,416,-56,-17179868716,17179869128,40,-8,24,6158430208,96,0,0,0,8,-8,-17179869184,17179869176,40,-8,24,6158430208,96,0,0,0,8,-8,-17179869184,17179869176,40,-8,24,6158430208,96,0,0,0,8,-8,-17179869184,17179869176,260,-15032385548,76,6158430247,28776,39,39,39,18,-15032385548,-17179869145,2147483636,1744830188,-15032385538,6979321756,52755359705,13602736,55834574809,55834574809,55834574809,27917287374,-15032385538,64424509401,-23622320130,-17045651416,8589934586,-16642998248,-21758857216,-17178820512,-27917287424,-27917287424,-27917287424,-15032385528,8589934586,-19327352832,-6,18790481644,-23622320132,23622320028,80672647129,17192423344,83751862233,83751862233,83751862233,42949672910,-23622320132,66571993049,-6442450948,
363849187
3000,6,number,7,8
905147;370|371.25|372.5|373.75|365|366.25|367.5|368.75;112|114|115|116|117|109|110|111;367.5|368.75|370|371.25|372.5|373.75|365|366.25;370;305"#
    );
}

/// Typed arrays' elements in compiled code (`tget_*`, `tset_*`) and the
/// interpreter's direct paths: every kind, ints and doubles stored, reads
/// and writes out of bounds, NaN, clamping, kinds mixed at one site. The
/// expected values are Node's.
const TYPED: &str = r#"
function typedArrays() {
function Heap(n) {
  this.buf = new ArrayBuffer(n * 8);
  this.i8 = new Int8Array(n); this.u8 = new Uint8Array(n); this.c8 = new Uint8ClampedArray(n);
  this.i16 = new Int16Array(n); this.u16 = new Uint16Array(n); this.i32 = new Int32Array(n);
  this.u32 = new Uint32Array(n); this.f32 = new Float32Array(n); this.f64 = new Float64Array(n);
}
Heap.prototype.fill = function (n, k) {
  for (var i = 0; i < n; i++) {
    var v = (i * 2654435761 + k) | 0, d = v / 7;
    this.i8[i] = v; this.u8[i] = v; this.c8[i] = v; this.i16[i] = v; this.u16[i] = v;
    this.i32[i] = v; this.u32[i] = v; this.f32[i] = d; this.f64[i] = d;
    if (i & 1) { this.i8[i] = d; this.i16[i] = d * 1000; this.i32[i] = d * 1e6; this.u32[i] = -d; this.f32[i] = v; this.f64[i] = v; }
  }
};
Heap.prototype.sum = function (n) {
  var s = 0, f = 0;
  for (var i = 0; i < n; i++) {
    s = (s + this.i8[i] + this.u8[i] + this.c8[i] + this.i16[i] + this.u16[i] + this.i32[i]) | 0;
    f += this.u32[i] + this.f32[i] + this.f64[i];
  }
  return s + ":" + f;
};
function oob(a, n) { var s = 0, u = 0; for (var i = 0; i < n; i++) { var v = a[i]; if (v === undefined) u++; else s += v; a[i + 3] = i; } return s + "/" + u; }
function mixed(arrs, n) { var s = 0; for (var i = 0; i < n; i++) { var a = arrs[i % arrs.length]; s += a[i & 7]; a[(i + 1) & 7] = i * 1.5; } return s; }
var h = new Heap(64), out = [];
for (var k = 0; k < 300; k++) { h.fill(64, k); var sm = h.sum(64); if (k % 50 == 0) out.push(sm); }
var small = new Int32Array(10), r = "";
for (var k2 = 0; k2 < 2000; k2++) r = oob(small, 12);
out.push(r, small.join(","));
var nanf = new Float64Array(4), nan8 = new Uint8Array(4), nanc = new Uint8ClampedArray(4);
for (var k3 = 0; k3 < 3000; k3++) { var x = k3 % 3 ? 0 / 0 : 1e10 + k3; nanf[k3 & 3] = x; nan8[k3 & 3] = x; nanc[k3 & 3] = k3 % 2 ? 300 : -5; }
out.push(nanf.join(","), nan8.join(","), nanc.join(","));
var ms = 0;
for (var k4 = 0; k4 < 300; k4++) ms = mixed([new Uint8Array(8), new Float32Array(8), new Int16Array(8), [1,2,3,4,5,6,7,8]], 64);
out.push(ms);
return out.join("\n");
}
"#;

#[test]
fn typed_array_elements_agree_with_node() {
    assert_eq!(
        value(&format!("{TYPED}\ntypedArrays();")),
        r#"-1174860914:117237799293.35715
-946382236:117237804373.07144
-717706432:117237808880.78572
-489031403:117237814869.5
-260552215:117237819201.71428
-32139072:117237824306.42857
21/2
0,0,0,0,1,2,3,4,5,6
NaN,10000002997,NaN,NaN
0,181,0,0
0,255,0,255
96"#
    );
}

/// Functions with upvalues in compiled code: read and written, in the
/// compiled function (through its frame's function) and inlined (the
/// cells known), closures of one code with other cells, an Emscripten-like
/// module whose functions share a heap, and `-x` and `~x`. The expected
/// values are Node's.
const UPVALUES: &str = r#"
function upvalues() {
function makeCounter(start) {
  var n = start, log = [];
  function inc(k) { n += k; return n; }
  function get() { return n; }
  function sum(arr) { var s = 0; for (var i = 0; i < arr.length; i++) s += inc(arr[i]) & 7; return s; }
  function mk(j) { return function () { return n + j; }; }
  return { inc: inc, get: get, sum: sum, mk: mk };
}
var a = makeCounter(0), b = makeCounter(1000);
var arr = []; for (var i = 0; i < 50; i++) arr.push(i % 7);
var t = 0;
for (var r = 0; r < 3000; r++) {
  t = (t + a.sum(arr) + b.sum(arr)) | 0;
  if (r % 500 == 0) t += a.mk(r)() - b.get();
}
var Mod = (function () {
  var HEAP = new Int32Array(1024), base = 16, calls = 0;
  function store(p, v) { HEAP[(p + base) >> 2] = v; calls++; }
  function load(p) { return HEAP[(p + base) >> 2]; }
  function run(n) { var s = 0; for (var i = 0; i < n; i++) { store(i * 4, i * 3); s = (s + load(i * 4)) | 0; if (i == n - 1) base = (base + 4) & 1023; } return s; }
  return { run: run, calls: function () { return calls; } };
})();
var m = 0; for (var q = 0; q < 2000; q++) m = (m + Mod.run(200)) | 0;
function outer() { var x = 1; function mid() { var y = 2; function inner() { x++; return x * 10 + y; } return inner; } return mid(); }
var f = outer(), z = 0; for (var q2 = 0; q2 < 5000; q2++) z = (z + f()) | 0;
return [t, a.get(), b.get(), m, Mod.calls(), z].join(",");
}
function negation() {
function f(a, b) { var s = 0, t = 0, u = []; for (var i = 0; i < 2000; i++) { var x = (i - 1000) * a; s += -x + ~(i * b); t += -(x / 3); if (i == 1000) u.push(1 / -x, -(-2147483648), ~2.7, -"5", ~"9"); } return [s, t, u.join(",")].join(";"); }
var r; for (var k = 0; k < 200; k++) r = f(k & 3, k);
return r + "|" + f(0.5, 1) + "|" + f(-1, -1);
}
"#;

#[test]
fn upvalues_and_negation_agree_with_node() {
    assert_eq!(value(&format!("{UPVALUES}\nupvalues();")), "1051500,441000,442000,119400000,400000,125085000");
    assert_eq!(
        value(&format!("{UPVALUES}\nnegation();")),
        "-397800000;1000;-Infinity,2147483648,-3,-5,-10|-2000500;166.66666666667243;-Infinity,2147483648,-3,-5,-10|1996000;-333.33333333334485;Infinity,2147483648,-3,-5,-10"
    );
}

/// Typed arrays in globals past the global object's in-object slots (as
/// Mandreel's `HEAPU16`): constants in compiled code, with a keyed store
/// in the same function; then replaced by arrays of other kinds. The
/// expected value is Node's.
const GLOBAL_HEAPS: &str = r#"
function globalHeaps() {
for (var g = 0; g < 40; g++) this["pad" + g] = g;
this.HEAPU16 = new Uint16Array(256); this.HEAP32 = new Int32Array(128);
function copy(n) { var s = 0; for (var i = 0; i < n; i++) { HEAPU16[i] = i * 7 + 65530; HEAP32[i & 127] = HEAPU16[i] - i; s = (s + HEAP32[i & 127]) | 0; } return s; }
var t = 0, r = [];
for (var k = 0; k < 3000; k++) t = (t + copy(200)) | 0;
r.push(t);
this.HEAPU16 = new Int8Array(256);
for (var k2 = 0; k2 < 300; k2++) t = (t + copy(200)) | 0;
r.push(t);
this.HEAP32 = new Float64Array(128);
HEAPU16.extra = 1;
for (var k3 = 0; k3 < 300; k3++) t = (t + copy(200)) | 0;
r.push(t, HEAPU16[3], HEAP32[3], HEAPU16.extra);
return r.join(",");
}
"#;

#[test]
fn global_typed_arrays_agree_with_node() {
    assert_eq!(value(&format!("{GLOBAL_HEAPS}\nglobalHeaps();")), "551208000,545503200,539798400,15,-244,1");
}

/// Keyed stores through the runtime (`set_keyed_rt`) into an object's
/// own elements, stored in place while their type fits, then values that
/// don't (a double, a string, an object; past the end). The expected
/// value is Node's.
const OWN_ELEMENTS: &str = r#"
function ownElements() {
function Big(n) { for (var i = 0; i < n; i++) this[i] = 0; this.t = n; }
function fill(o, n, v) { for (var i = 0; i < n; i++) o[i] = v + i; }
function sum(o, n) { var s = 0; for (var i = 0; i < n; i++) s += typeof o[i] == "number" ? o[i] : o[i].length; return s; }
var a = new Big(40), b = new Big(40), r = [];
for (var k = 0; k < 2000; k++) { fill(a, 40, k); fill(b, 30, k & 7); }
r.push(sum(a, 40), sum(b, 40));
fill(a, 40, 0.5); r.push(sum(a, 40));
fill(b, 45, "x"); r.push(sum(b, 45), b[44], b.t);
var arr = [1, 2, 3]; for (var k2 = 0; k2 < 500; k2++) fill(arr, 3, k2); fill(arr, 3, {length: 9}); r.push(sum(arr, 3));
return r.join(",");
}
"#;

#[test]
fn own_element_stores_agree_with_node() {
    assert_eq!(value(&format!("{OWN_ELEMENTS}\nownElements();")), "80740,645,800,125,x44,40,48");
}

/// ToInt32 of doubles around 2^31, 2^32, 2^63 and the non-finite ones;
/// int32s and doubles stored into every typed array kind. The expected
/// value is Node's.
const TO_INT32: &str = r#"
function toInt32s() {
var ds = [0.5, -0.5, -1.5, 2147483647.9, 2147483648.5, -2147483649.5, 4294967296.5, 4294967301.7, -4294967301.7, 1e15 + 0.5, -1e18, 9.2e18, 9223372036854775808, -9223372036854775808, 1e19, 1.8e308, -0, NaN, Infinity, -Infinity];
var r = [];
for (var i = 0; i < ds.length; i++) r.push(ds[i] | 0, ds[i] >>> 0, ~ds[i]);
var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
var vs = [-129, -1, 0, 200, 255, 256, 40000, 70000, -70000, 2147483647, -2147483648, 16777217];
for (var k = 0; k < kinds.length; k++) {
  var a = new kinds[k](vs.length);
  for (var j = 0; j < vs.length; j++) a[j] = vs[j];
  for (var j2 = 0; j2 < vs.length; j2++) r.push(a[j2]);
  a[0] = 1e10 + 3.7; a[1] = -3.5; r.push(a[0], a[1]);
}
return r.join(",");
}
"#;

#[test]
fn to_int32_agrees_with_node() {
    assert_eq!(
        value(&format!("{TO_INT32}\ntoInt32s();")),
        "0,0,-1,0,0,-1,-1,4294967295,0,2147483647,2147483647,-2147483648,-2147483648,2147483648,2147483647,2147483647,2147483647,-2147483648,0,0,-1,5,5,-6,-5,4294967291,4,-1530494976,2764472320,1530494975,1486618624,1486618624,-1486618625,-1650982912,2643984384,1650982911,0,0,-1,0,0,-1,-1981284352,2313682944,1981284351,0,0,-1,0,0,-1,0,0,-1,0,0,-1,0,0,-1,127,-1,0,-56,-1,0,64,112,-112,-1,0,1,3,-3,127,255,0,200,255,0,64,112,144,255,0,1,3,253,0,0,0,200,255,255,255,255,0,255,0,255,255,0,-129,-1,0,200,255,256,-25536,4464,-4464,-1,0,1,-7165,-3,65407,65535,0,200,255,256,40000,4464,61072,65535,0,1,58371,65533,-129,-1,0,200,255,256,40000,70000,-70000,2147483647,-2147483648,16777217,1410065411,-3,4294967167,4294967295,0,200,255,256,40000,70000,4294897296,2147483647,2147483648,16777217,1410065411,4294967293,-129,-1,0,200,255,256,40000,70000,-70000,2147483648,-2147483648,16777216,10000000000,-3.5,-129,-1,0,200,255,256,40000,70000,-70000,2147483647,-2147483648,16777217,10000000003.7,-3.5"
    );
}

/// Shifts of a value of no known type (an array of ints and strings),
/// compiled checked to be an int32 until a double, a string, an object or
/// `null` comes; stores of numbers into an array of known shape. The
/// expected value is Node's.
const UNTYPED_SHIFTS: &str = r#"
function untypedShifts() {
var a = [], r = [];
for (var i = 0; i < 64; i++) a.push(i % 3 == 0 ? "s" : i);
function f(a, n) { var s = 0; for (var i = 1; i < n; i++) { if (i % 3) s = (s + (a[i] >> 2) + (a[i] << 3)) | 0; } return s; }
function g(n, x) { var s = 0, b = [x, 1 << 20, -12345, 7]; for (var i = 0; i < n; i++) s = (s + (b[i & 3] >> 1) + (b[i & 3] << 2)) | 0; return s; }
for (var k = 0; k < 300; k++) r[0] = f(a, 64);
a[4] = 1e10 + 0.5; a[5] = -7.9; r.push(f(a, 64));
a[7] = "12"; a[8] = {}; r.push(f(a, 64));
for (var k2 = 0; k2 < 300; k2++) r[3] = g(100, 3);
r.push(g(100, 2.5), g(100, "40"), g(100, null), g(100, true));
var arr = [1.5, 2.5]; for (var k3 = 0; k3 < 2000; k3++) arr[k3 & 1] = k3 / 4; r.push(arr.join(":"));
return r.join(",");
}
"#;

#[test]
fn untyped_shifts_agree_with_node() {
    assert_eq!(
        value(&format!("{UNTYPED_SHIFTS}\nuntypedShifts();")),
        "10899,-1251851505,-1251851529,116577075,116576975,116581250,116576750,116576850,499.5:499.75"
    );
}

#[test]
fn exits_at_an_instruction_never_compiled_dont_lower_again() {
    // `block`'s getter in an object literal is never compiled: exits
    // there lower it again only at the 1st, 2nd, 4th… (Octane RegExp's
    // `runBlock1` was lowered at every call, at a regular expression
    // literal, compiled since).
    let src = "function block(n) {
  var o = { get x() { return 1; } }, d = o.x === 1, s = 0;
  for (var i = 0; i < n; i++) s += i;
  return s + (d ? 1 : 0);
}
var t = 0;
for (var k = 0; k < 300; k++) t += block(50);
t;";
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(20);
    assert_eq!(display(vm.run(&m).unwrap()), "367800");
    let st = vm.jit_stats("block").expect("block compiled");
    // (11 for 288 exits here; one per exit before.)
    assert!(st.lowers <= 16 && st.exits > 100, "{st:?}");
}

#[test]
fn a_call_that_had_not_run_is_compiled_once_it_has() {
    // `f`'s loop compiles before its call has run: the call is left to the
    // interpreter, and then compiled, not left at every call of `f`.
    let src = "function g(x) { return x * 2; }
function f(n, h) {
  var s = 0;
  for (var i = 0; i < n; i++) {
    if (i == 50) s += h(i);
    s += i;
  }
  return s;
}
var t = 0;
for (var k = 0; k < 300; k++) t += f(100, g);
t;";
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(20);
    assert_eq!(display(vm.run(&m).unwrap()), "1515000");
    let st = vm.jit_stats("f").expect("f compiled");
    // (1 here; 20 when the exit at the call left it uncompiled.)
    assert!(st.exits <= 5, "{st:?}");
}

#[test]
fn an_intrinsic_leaves_a_script_s_loops_compiled() {
    // `class {}` and the destructuring are intrinsics: the script's loop
    // is compiled all the same, and `g`, which it calls (#125: neither
    // was; nothing logged with `CLOISON_JIT_LOG`).
    let src = "var C = class {};
var [p, q] = [1, 2];
function g(i) { return i + p; }
var s = 0;
for (var i = 0; i < 100000; i++) s += g(i);
s + q;";
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(1000);
    assert_eq!(display(vm.run(&m).unwrap()), "5000050002");
    let st = vm.jit_stats("<script>").expect("the script compiled");
    assert!(st.compiles >= 1, "{st:?}");
}

/// An opcode the JIT leaves to the interpreter on execution must not keep an
/// ordinary function interpreted when it is on a cold path (#241).
#[test]
fn cold_interpreter_opcode_does_not_prevent_function_entry() {
    // (Large enough to be entered: see `vm::INTRINSIC_ENTRY_SIZE`.)
    let body = "s += n; ".repeat(200);
    let src = format!(
        "function f(n) {{ if (n < 0) `${{n}}`; var s = 0; {body} return s; }}\
         var out = 0; for (var i = 0; i < 20; i++) out += f(i); out;"
    );
    let m = cloison::js::compile(&src).unwrap();
    assert!(cloison::asm::disassemble(&m).contains("Intrinsic"), "{src}");
    let mut vm = Vm::new();
    vm.set_tier_back_edges(2);
    assert_eq!(display(vm.run(&m).unwrap()), (200 * 190).to_string());
    let st = vm.jit_stats("f").expect("f entered compiled code");
    assert!(st.compiles >= 1 && st.exits == 0, "{st:?}");
}

/// Interpreter-only bytecodes may compile at an ordinary entry or a loop, but
/// their fallback needs the header that direct and inlined calls do not make.
#[test]
fn interpreter_frame_opcodes_are_not_called_directly() {
    let src = "function rest(a, ...xs) {
  var s = 0; for (var i = 0; i < 20; i++) s += a; return s + xs.length;
}
function mapped(a) {
  var s = 0; for (var i = 0; i < 20; i++) s += a;
  arguments[0] = a + 1; return s + arguments[0];
}
var callee = function self(n) {
  if (n < 0) return self === callee;
  var s = 0; for (var i = 0; i < 20; i++) s += n; return s;
};
function C(n) {
  this.ok = new.target === C;
  var s = 0; for (var i = 0; i < 20; i++) s += n; this.x = s;
}
function hot(n) {
  var s = 0;
  for (var i = 0; i < n; i++) s += rest(i, 1) + mapped(i) + callee(i) + new C(i).x;
  return s;
}
var total = 0;
for (var run = 0; run < 20; run++) total += hot(30);
[rest(-1, 7, 8), mapped(-1), callee(-1), new C(-1).ok, total].join(',');";
    let m = cloison::js::compile(src).unwrap();
    let asm = cloison::asm::disassemble(&m);
    for op in ["CreateRest", "CreateMappedArguments", "LdaCallee", "LdaNewTarget"] {
        assert!(asm.contains(op), "missing {op} in {src}");
    }
    let mut vm = Vm::new();
    vm.set_tier_back_edges(2);
    assert_eq!(display(vm.run(&m).unwrap()), "-18,-20,true,true,705900");
    assert!(vm.jit_stats("hot").is_some(), "hot did not compile");
    // (Each runs interpreted: see `mapped_arguments_see_parameters_…` and
    // `prologue_exits_keep_functions_interpreted`.)
    assert_eq!(vm.jit_bound_direct_calls("hot"), 0);
}

/// A function with a loop and an intrinsic is not entered compiled at its
/// calls until its loop tiers up (Octane CodeLoad's `each`, a copy at each
/// load with a short `for … in` and a spread: -14% compiled at its calls).
#[test]
fn intrinsic_with_short_loop_is_not_entered_at_calls() {
    let src = "function each(o) { var n = 0; for (var k in o) n++; return Math.max(...[n, 1]); }
var t = 0;
for (var i = 0; i < 200; i++) t += each({a: 1, b: 2});
t;";
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    // (Its loop's 400 turns in all, short of tiering up; calls would
    // enter after 10.)
    vm.set_tier_back_edges(1000);
    assert_eq!(display(vm.run(&m).unwrap()), "400");
    assert!(vm.jit_stats("each").is_none(), "{:?}", vm.jit_stats("each"));
}

/// A function whose prologue exits compiled code (a rest parameter, its
/// own name, `new.target`) runs interpreted: entered at its calls, it
/// would leave at once, and its short loops entered at each call cost
/// more than they win.
#[test]
fn prologue_exits_keep_functions_interpreted() {
    let src = "function rest(a, ...xs) { var s = 0; for (var i = 0; i < 30; i++) s += a; return s + xs.length; }
var self_ = function me(a) { var s = 0; for (var i = 0; i < 30; i++) s += a; return me === self_ ? s : -1; };
function C(a) { var s = 0; for (var i = 0; i < 30; i++) s += a; this.s = new.target === C ? s : -1; }
var t = 0;
for (var k = 0; k < 2000; k++) t += rest(1, 2, 3) + self_(1) + new C(1).s;
t;";
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(2);
    assert_eq!(display(vm.run(&m).unwrap()), (2000 * 92).to_string());
    for function in ["rest", "me", "C"] {
        assert!(vm.jit_stats(function).is_none(), "{function}: {:?}", vm.jit_stats(function));
    }
}

/// A mapped `arguments` aliases its frame's parameter registers: code
/// tiering up at a loop after it would keep its own copies of them.
#[test]
fn mapped_arguments_see_parameters_assigned_in_loops() {
    let src = "function inc(a) {
  for (var i = 0; i < 300; i++) a++;
  return arguments[0];
}
function store(a) {
  var s = 0;
  for (var i = 0; i < 300; i++) { arguments[0] = i; s += a; }
  return s;
}
function str(a) {
  var s;
  for (var i = 0; i < 300; i++) { arguments[0] = 'x'; s = a + 1; }
  return s;
}
var r = [];
for (var k = 0; k < 20; k++) r = [inc(5), store(1), str(5)];
r.join(',');";
    for tier in [1, 2, 1000] {
        let m = cloison::js::compile(src).unwrap();
        let mut vm = Vm::new();
        vm.set_tier_back_edges(tier);
        assert_eq!(display(vm.run(&m).unwrap()), "305,44850,x1", "tier {tier}");
    }
}

#[test]
fn intrinsics_in_compiled_loops_throw_and_return_as_interpreted() {
    // Each loop compiled, with intrinsics (spread, templates, `**`,
    // destructuring, classes, private names) in it, some of them throwing
    // at times; the same values as interpreted.
    let src =
        "class A { #p = 1; constructor(x) { this.x = x; } get d() { return this.x * 2; } pget(o) { return o.#p; } }
class B extends A { constructor(x) { super(x); this.y = x + 1; } m() { return super.d + this.y; } }
function h(a, b, c) { return a + b + c; }
function f(n) {
  var s = 0, t = '', errs = 0;
  for (var i = 0; i < n; i++) {
    var [x, y = 2] = [i];
    var { a, ...rest } = { a: i, b: 1 };
    s += x + y + a + rest.b + i ** 2 % 3 + h(...[i, 1, 2]);
    t = `${x}-${y}`;
    try { if (i % 100 == 50) { var [w] = {}; } } catch (e) { errs++; }
    try { if (i % 100 == 60) A(); } catch (e) { errs++; }
  }
  return s + t + errs;
}
var r = '';
for (var k = 0; k < 20; k++) r = f(1000 + k);
var acc = 0, errs = 0;
for (var i = 0; i < 5000; i++) {
  var b = new B(i);
  var [u, v] = [i, b.m()];
  acc += u + v + b.d;
  try { b.pget({}); } catch (e) { errs++; }
  try { var { z } = null; } catch (e) { errs++; }
}
function thrower(n) { for (var i = 0; i < n; i++) { var [x] = i < n - 1 ? [i] : 0; } return 'no'; }
var caught = 0;
for (var k = 0; k < 20; k++) { try { thrower(2000); } catch (e) { if (e instanceof TypeError) caught++; } }
[r, acc, errs, caught].join();";
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(100);
    let v = display(vm.run(&m).unwrap());
    assert_eq!(v, "15628061018-220,74990000,10000,20");
    assert!(vm.jit_stats("f").is_some_and(|s| s.compiles >= 1));
    assert!(vm.jit_stats("thrower").is_some_and(|s| s.compiles >= 1));
}

/// Runs `src` tiering up after 1000 back edges (the default, whatever
/// `CLOISON_TIER` says); returns its value and `function`'s stencils.
fn compiled(src: &str, function: &str) -> (String, Vec<String>) {
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(1000);
    let v = display(vm.run(&m).unwrap());
    (v, vm.jit_stencils(function).unwrap_or_default())
}

const CONSTANT_FIELD_COMPARES: &str = "var K = { ONE: 1, TWO: 2 };
function V(d, n) { this.d = d; this.n = n; }
function f(a, n) {
  var s = 0;
  for (var i = 0; i < n; i++) {
    var o = a[i % a.length];
    if (o.n.d == K.ONE) s += 2;
    if (o.d == K.ONE) s += 5; else s -= 1;
    if (o.d != K.TWO) s += 3;
    var t = o.d;
    if (t == K.TWO) s += t;
  }
  return s;
}
var a = [new V(1, null), new V(2, null), new V(0, null)];
a[0].n = a[1]; a[1].n = a[2]; a[2].n = a[0];
var r = 0;
for (var k = 0; k < 300; k++) r += f(a, 31);
";

/// An int32 field compared with a constant one (DeltaBlue's `this.direction
/// == Direction.FORWARD`, the constant folded): `Star t; LdaSmi k;
/// TestEqual t` compares `acc` with `k` (`teq_smi_jf_i`), and the `Star`
/// goes if `t` is dead after. Not where the load before the `Star` loads
/// into `t`'s slot instead of `acc` (`get_own_nc_g2_to_g1`): `acc` doesn't
/// hold `t` there (the second and third compares in `f`, whose temporary
/// also holds `o.n`). Then a double, and code without them.
#[test]
fn compares_of_fields_with_constant_fields() {
    let (v, s) = compiled(&format!("{CONSTANT_FIELD_COMPARES}r;"), "f");
    assert_eq!(v, "41400");
    assert!(s.iter().any(|n| n.contains("teq_smi_jf_i")), "{s:?}");
    assert!(s.iter().any(|n| n.contains("_to_g")), "{s:?}");
    let src = format!("{CONSTANT_FIELD_COMPARES}a[2].d = 1.5;\nfor (var k = 0; k < 50; k++) r += f(a, 31);\nr;");
    assert_eq!(value(&src), "48300");
}

/// `Ldar a; Star b` with `acc` dead after as one move between slots and
/// the frame (`enter_*`, `spill_*`, `mov_u*`), and loads into a slot
/// (`get_own_nc_g0_to_g2`): more locals than slots, rotated each
/// iteration; exits on entering mid-loop leave the interpreter the moved
/// values.
#[test]
fn moves_between_slots_and_frame() {
    let src = "function P(x, y) { this.x = x; this.y = y; this.n = null; }
function g(ps, n) {
  var a = 0, b = 1, c = 2, d = 3, e = 4, f = 5, h = 6, j = 7, s = 0;
  for (var i = 0; i < n; i++) {
    var p = ps[i % ps.length];
    var q = p.n, x = p.x, y = p.y;
    var t = a; a = b; b = c; c = d; d = e; e = f; f = h; h = j; j = t;
    var u = x; x = y; y = u;
    s += x - y + a * 2 - j + (q === null ? 1 : q.x);
  }
  return s + a + b + c + d + e + f + h + j;
}
var ps = [new P(1, 2), new P(3, 5), new P(8, 13)];
ps[1].n = ps[2]; ps[2].n = ps[0];
var r = 0;
for (var k = 0; k < 300; k++) r += g(ps, 29);
r;";
    assert_eq!(value(src), "91800");
    let (_, s) = compiled(src, "g");
    for kind in ["enter_", "spill_", "mov_u", "_to_g"] {
        assert!(s.iter().any(|n| n.contains(kind)), "{kind}: {s:?}");
    }
}

/// A small function whose calls are all compiled inline is inlined past
/// the inlining depth: `add` pushing (DeltaBlue's `OrderedCollection.add`)
/// three calls down, and `weakestOf` calling the leaf `weaker`.
#[test]
fn small_functions_calling_push_or_leaves_are_inlined_deep() {
    let src = "function C() { this.e = []; }
C.prototype.add = function (x) { this.e.push(x); };
C.prototype.size = function () { return this.e.length; };
function Plan() { this.v = new C(); }
Plan.prototype.addC = function (c) { this.v.add(c); };
function S(w) { this.w = w; }
S.weaker = function (a, b) { return a.w > b.w; };
S.weakestOf = function (a, b) { return this.weaker(a, b) ? a : b; };
function fill(p, xs) {
  for (var i = 0; i < xs.length; i++) p.addC(S.weakestOf(xs[i], xs[(i + 1) % xs.length]));
}
function outer(xs) {
  var p = new Plan();
  for (var k = 0; k < 3; k++) fill(p, xs);
  var s = 0;
  for (var i = 0; i < p.v.size(); i++) s += p.v.e[i].w;
  return s;
}
var xs = [new S(4), new S(1), new S(7), new S(3)];
var r = 0;
for (var k = 0; k < 400; k++) r += outer(xs);
r;";
    assert_eq!(value(src), "26400");
    let (_, s) = compiled(src, "outer");
    assert!(s.iter().any(|n| n.contains("array_push")), "{s:?}");
    assert!(!s.iter().any(|n| n.contains("call_direct")), "{s:?}");
}

/// A small constructor making objects with small constructors and empty
/// arrays (`new Array()`, compiled inline) is inlined past the inlining
/// depth: `new Plan()` in `extract`, inlined into `change`, inlined into
/// `main` (the short-lived-per-call micro; DeltaBlue's `Plan` and
/// `OrderedCollection` are alike).
#[test]
fn small_constructors_making_objects_are_inlined_deep() {
    let src = "function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
function Edit(target) { this.target = target; this.active = true; }
function Plan() { this.v = new Coll(); }
Plan.prototype.add = function (x) { this.v.add(x); };
Plan.prototype.run = function (value) {
  for (var i = 0; i < this.v.size(); i++) this.v.at(i).target.value = value;
};
function V() { this.value = 0; }
function extract(edits) {
  var plan = new Plan();
  for (var i = 0; i < edits.size(); i++) if (edits.at(i).active) plan.add(edits.at(i));
  return plan;
}
function change(v, x) {
  var edit = new Edit(v);
  var edits = new Coll();
  edits.add(edit);
  var plan = extract(edits);
  for (var i = 0; i < 10; i++) plan.run(x + i);
  edit.active = false;
}
function main() {
  var v = new V(), total = 0;
  for (var r = 0; r < 3000; r++) { change(v, r); total += v.value; }
  return total;
}
main();";
    assert_eq!(value(src), "4525500");
    let (_, s) = compiled(src, "main");
    assert!(s.iter().any(|n| n.contains("create_array")), "{s:?}");
    assert!(!s.iter().any(|n| n.contains("construct_direct")), "{s:?}");
}

/// Named loads and stores whose sites gave up on the stencils go through
/// the runtime: a native getter (`byteLength`) is called there, an
/// accessor of bytecode functions leaves to the interpreter.
#[test]
fn megamorphic_named_accesses_through_the_runtime() {
    let src = r#"
function mk(i) {
  var o = {}; o["k" + (i % 13)] = i; o.v = i;
  if (i % 17 == 0) Object.defineProperty(o, "w", { get: function () { return 1000; }, set: function (x) { this.v = -x; } });
  return o;
}
var objs = []; for (var i = 0; i < 100; i++) objs.push(mk(i));
var bufs = []; for (var i = 0; i < 9; i++) { var b = new Uint8Array(i + 1); b["p" + i] = i; bufs.push(b); }
function run(a, b) {
  var s = 0;
  for (var j = 0; j < a.length; j++) { var o = a[j]; s += o.v; o.w = j; s += o.w; }
  for (var j = 0; j < b.length; j++) s += b[j].byteLength;
  return s;
}
var t = 0; for (var r = 0; r < 500; r++) t += run(objs, bufs);
t;"#;
    assert_eq!(value(src), "7590510");
    let (_, s) = compiled(src, "run");
    assert!(s.iter().any(|n| n.starts_with("get_named_rt")), "{s:?}");
}

/// An error message shows a long string cut (#65): a TypeError for
/// calling a string of 2^30 units is caught as one, its message short,
/// and leaves nothing pending to throw later (the message, made whole,
/// was past the longest string). The cons string (2 MiB, its pieces
/// shared) isn't flattened.
#[test]
fn error_messages_cut_long_strings() {
    let src = r#"
var s = "x".repeat(2 ** 20);
for (var i = 0; i < 10; i++) s = s + s;
var sym = Symbol(s);
var out = [];
function t(name, f) {
    try { f(); out.push(name + ":none"); }
    catch (e) { out.push(name + ":" + e.name + ":" + e.message.length + ":" + e.message.slice(-20)); }
}
t("call", () => s());
t("new", () => new s());
t("forEach", () => [].forEach(s));
t("promise", () => new Promise(s));
t("keyFor", () => Symbol.keyFor(s));
t("in", () => "a" in s);
t("iterable", () => { for (var x of sym) {} });
t("symbol call", () => sym());
out.push("after");
var p = Promise.reject(s).then(undefined, s);
p.then(undefined, (e) => out.push("job:" + e.name + ":" + e.message.length));
out.push(String(1.5) + [2].join());
out.join();"#;
    assert_eq!(
        light_value(src),
        "call:TypeError:1021:.. is not a function,new:TypeError:1024:is not a constructor,\
         forEach:TypeError:1047:y.prototype.forEach),promise:TypeError:1038:.. is not a function,\
         keyFor:TypeError:1019:x... is not a symbol,in:TypeError:1049:xxxxxxxxxxxxxxxxx...,\
         iterable:TypeError:1027:...) is not iterable,symbol call:TypeError:1029:.) is not a function,\
         after,1.52"
    );
}

/// `throw` of a long string doesn't copy its units out for its error's
/// message (#145; see the next test): caught, it is the same string, a
/// cons string of 2^30 units (2 MiB, its pieces shared), not flattened.
#[test]
fn throwing_a_long_string_keeps_it_whole() {
    let src = r#"
var s = "x".repeat(2 ** 20);
for (var i = 0; i < 10; i++) s = s + s;
var out = [];
try { throw s; } catch (e) { out.push(e === s, e.length); }
function* g() { yield 1; }
var it = g();
it.next();
try { it.throw(s); } catch (e) { out.push(e === s); }
out.join();"#;
    assert_eq!(light_value(src), "true,1073741824,true");
}

/// An uncaught `throw` of a string shows it whole, though its error's
/// message is cut past 1000 code units (#145).
#[test]
fn an_uncaught_long_string_is_shown_whole() {
    let module = cloison::js::compile(r#"throw "x".repeat(1500) + "end";"#).unwrap();
    let mut vm = Vm::new();
    let e = vm.run(&module).unwrap_err();
    assert!(e.to_string().ends_with("x..."), "{e}");
    assert_eq!(vm.uncaught_message(&e), "x".repeat(1500) + "end");
}

/// A function's name past the longest string throws a RangeError where
/// it is made (#65): a method named after a symbol (`[description]`,
/// made by an intrinsic, which left the error pending: it went off past
/// the `try`), a bound function's (`bound name`), and so does an error's
/// `toString`. Cons strings of about 2^30 units (2 MiB), never flattened.
#[test]
fn function_names_throw_past_the_longest_string() {
    let src = r#"
var p = "x".repeat(2 ** 20 - 1);
for (var i = 0; i < 10; i++) p = p + p;
var s = p + "x".repeat(2 ** 10 - 1);
var t = s + "x";
var sym = Symbol(s);
var out = [];
function t_(name, f) { try { out.push(name + ":" + f()); } catch (e) { out.push(name + ":" + e.name); } }
// (No call in these `try`s: one would throw what was left pending.)
var o, e1, e2, e3;
try { o = { [sym]() { return 1; } }; } catch (e) { e1 = e; }
try { class C { [sym]() {} } } catch (e) { e2 = e; }
try { class C { static [sym]() {} } } catch (e) { e3 = e; }
out.push("method:" + typeof o + ":" + e1.name, "class:" + e2.name, "static:" + e3.name);
t_("short", () => { var o = { [Symbol("d")]() {} }; return o[Object.getOwnPropertySymbols(o)[0]].name; });
t_("bound", () => { function f() {} Object.defineProperty(f, "name", { value: t }); return f.bind().name.length; });
t_("bound fits", () => { function f() {} Object.defineProperty(f, "name", { value: "x" }); return f.bind().name; });
t_("toString", () => Error.prototype.toString.call({ name: "E", message: t }).length);
out.push("after");
out.push([1, 2].join("+"));
out.join();"#;
    assert_eq!(
        light_value(src),
        "method:undefined:RangeError,class:RangeError,static:RangeError,short:[d],bound:RangeError,\
         bound fits:bound x,toString:RangeError,after,1+2"
    );
}

/// More builders throw past the longest string before they read a long
/// piece (#65): `replace`'s, `replaceAll`'s and `RegExp`'s
/// `Symbol.replace`'s function results, `%TypedArray%` `join`'s
/// separators, JSON's object members, a symbol's string; and JSON's
/// gap: its separators' indentation counted before it is written. Each
/// error caught where it is thrown, nothing left pending past the `try`.
/// Cons strings of 2^30 units (2 MiB), never flattened.
#[test]
fn more_builders_throw_past_the_longest_string() {
    let src = r#"
var s = "x".repeat(2 ** 20);
for (var i = 0; i < 10; i++) s = s + s;
var h = "x".repeat(2 ** 19);
for (var i = 0; i < 10; i++) h = h + h;
h = h + "x";
var deep = Array(107400).fill(0);
for (var i = 0; i < 1000; i++) deep = [deep];
var out = [];
function t(name, f) { var r; try { r = f(); } catch (e) { r = e.name; } out.push(name + ":" + r); }
t("replace", () => "xa".replace("a", () => s));
t("replace fits", () => "xa".replace("a", () => "yz"));
t("replaceAll", () => "xaa".replaceAll("a", () => s));
t("Symbol.replace", () => /a/[Symbol.replace]("xa", () => s));
t("Symbol.replace fits", () => /a/g[Symbol.replace]("xaa", () => "yz"));
t("typed join", () => new Uint8Array(3).join(h));
t("typed join fits", () => new Uint8Array(3).join("-"));
t("object member", () => JSON.stringify({ a: s }));
t("raw", () => JSON.stringify([JSON.rawJSON("1"), JSON.rawJSON("2")]));
t("gap", () => JSON.stringify(deep, null, 10));
t("gap fits", () => JSON.stringify([[0, 0]], null, 1).length);
t("symbol toString", () => Symbol(s).toString());
t("String(symbol)", () => String(Symbol(s)));
t("symbol fits", () => String(Symbol("d")));
out.push("after");
out.push([1, 2].join("+"));
out.join();"#;
    assert_eq!(
        light_value(src),
        "replace:RangeError,replace fits:xyz,replaceAll:RangeError,Symbol.replace:RangeError,\
         Symbol.replace fits:xyzyz,typed join:RangeError,typed join fits:0-0-0,\
         object member:RangeError,raw:[1,2],gap:RangeError,gap fits:18,symbol toString:RangeError,\
         String(symbol):RangeError,symbol fits:Symbol(d),after,1+2"
    );
}

/// Checks of whether a value is a string don't flatten it (#146):
/// `String`, `String.prototype.toString`, the `stack` setter,
/// `Iterator.from`. Cons strings of 2^30 units (2 MiB), never flattened.
#[test]
fn string_checks_dont_flatten() {
    let src = r#"
var s = "x".repeat(2 ** 20);
for (var i = 0; i < 10; i++) s = s + s;
var e = new Error();
e.stack = s;
[String(s) === s, String(s).length, typeof new String(s), s.toString() === s,
 Object(s).toString() === s, String.prototype.toString.call(s) === s, e.stack === s,
 typeof Iterator.from(s).next].join();"#;
    assert_eq!(light_value(src), "true,1073741824,object,true,true,true,true,function");
}

/// Each binding of a generator's or an async function's block-scoped
/// variable that closures use is its own (each loop iteration's), kept in
/// a box (`js::codegen::boxed_lexicals`), and shared with the closures
/// across `yield`s and `await`s.
#[test]
fn generator_block_bindings_are_per_iteration() {
    let src = r#"
var r = [];
function* g1() { var fs = []; for (let i = 0; i < 3; i++) fs.push(() => i); yield fs.map(f => f()).join(""); }
r.push(g1().next().value);
function* g2() { var fs = []; for (let i = 0; i < 3; i++) { fs.push(() => i); yield i; i += 10; } return fs.map(f => f()).join("/"); }
var it = g2(), x; while (!(x = it.next()).done); r.push(x.value);
function* g3() { var fs = []; for (const v of ["a", "b", "c"]) { fs.push(function () { return v; }); yield; } return fs.map(f => f()).join(""); }
it = g3(); while (!(x = it.next()).done); r.push(x.value);
function* g4() { var fs = []; for (var k = 0; k < 3; k++) { let y = k * 2; fs.push(() => y++); } yield fs.map(f => f() + f()).join(","); }
r.push(g4().next().value);
function* g5() { let shared = 0; var inc = () => ++shared; yield inc(); yield inc() + shared; }
it = g5(); r.push(it.next().value, it.next().value);
function* g6() { var fs = []; for (let k in { p: 1, q: 2 }) fs.push(() => k); yield fs.map(f => f()).join(""); }
r.push(g6().next().value);
function* g7() { var out = []; for (let i = 0; i < 2; i++) { let c = i; var f = () => { c += 100; return c; }; yield f(); out.push(c); } return out.join(); }
it = g7(); var ys = []; while (!(x = it.next()).done) ys.push(x.value); r.push(ys.join(), x.value);
function* g8() { var fs = []; for (let i = 0; i < 3; i++) fs.push(function () { return function () { return i; }; }); yield fs.map(f => f()()).join(""); }
r.push(g8().next().value);
r.join();"#;
    assert_eq!(value(src), "012,10,abc,1,5,9,1,4,pq,100,101,100,101,012");
    let first = cloison::js::compile(
        r#"
var r = [];
async function a1() { var fs = []; for (let i = 0; i < 3; i++) { await null; fs.push(() => i); } return fs.map(f => f()).join(""); }
a1().then(v => { r.push(v); });
async function a2() { const t = 5; let u = 6; await 0; u++; return (() => t + u)(); }
a2().then(v => r.push(v));
"#,
    )
    .unwrap();
    let second = cloison::js::compile("r.join()").unwrap();
    for budget in [i64::MAX, 1, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run(&first).unwrap();
        let r = vm.run(&second).map(display).unwrap();
        assert_eq!(r, "12,012", "tier at {budget}");
    }
}

/// `charCodeAt` and `String.fromCharCode` inlined: flat and cons strings,
/// indices past the end and not int32s, units past 255 and arguments not
/// int32s, a receiver no string, and `charCodeAt` replaced.
#[test]
fn char_codes_agree_with_node() {
    let src = r#"
function sum(s) { var t = 0; for (var i = 0; i < s.length; i++) t += s.charCodeAt(i); return t; }
function sumd(s) { var t = 0; for (var i = 0; i < s.length; i += 0.5) t += s.charCodeAt(i) | 0; return t; }
function past(s) { var t = 0; for (var i = 0; i <= s.length; i++) { var c = s.charCodeAt(i); t += (c === c) ? c : 1000; } return t; }
function mk(n) { var o = ""; for (var i = 0; i < n; i++) o += String.fromCharCode(32 + (i * 7) % 300); return o; }
function mkd(n) { var o = ""; for (var i = 0; i < n; i++) o += String.fromCharCode(65.7 + i % 26); return o; }
function weird(xs) { var t = ""; for (var i = 0; i < xs.length; i++) t += String.fromCharCode(xs[i]); return t; }
function other(o) { var t = 0; for (var i = 0; i < 3; i++) t += o.charCodeAt(i); return t; }
function main() {
  var r = 0, s = "";
  for (var k = 0; k < 300; k++) {
    s = mk(50 + k % 7);
    r += sum(s) + sumd(s) + past(s) + sum(mkd(30));
    var c = "ab" + k; c = c + "cd"; r += sum(c);
    r += weird([65, "66", 67.9, -1, 65536 + 66, null]).length * 1000 + sum(weird([65, "66", 67.9, -1, 65536 + 66, null]));
  }
  var fake = { charCodeAt: function (i) { return i * 2; } };
  for (var k = 0; k < 2000; k++) r += other(k % 50 == 49 ? fake : "xyz");
  var saved = String.prototype.charCodeAt;
  String.prototype.charCodeAt = function (i) { return 1; };
  r += sum("hello");
  String.prototype.charCodeAt = saved;
  r += sum("hello");
  return r;
}
main();"#;
    assert_eq!(value(src), "33421313");
    let (_, s) = compiled(src, "sum");
    assert!(s.iter().any(|n| n.starts_with("char_code_at")), "{s:?}");
}

/// Keyed stores at sites that appended (`set_keyed_ap`): elements of
/// every type, past the capacity, holes, and receivers no array (an object,
/// a typed array, a named key).
#[test]
fn appending_stores_agree_with_node() {
    let src = r#"
function fill(a, n, f) { for (var i = 0; i < n; i++) a[a.length] = f(i); return a; }
function put(a, k, v) { a[k] = v; }
function sum(a) { var t = 0; for (var i = 0; i < a.length; i++) { var v = a[i]; t += typeof v == "number" ? v : (v === undefined ? 7 : (v && v.length !== undefined ? v.length : 3)); } return t; }
function main() {
  var r = 0;
  for (var k = 0; k < 200; k++) {
    var a = fill([], 40 + k % 13, function (i) { return i; });
    fill(a, 5, function (i) { return i + 0.5; });
    fill(a, 3, function (i) { return "s" + i; });
    fill(a, 2, function (i) { return { x: i }; });
    r += sum(a) + a.length;
    var b = [1, 2, 3];
    put(b, 5, 9); put(b, 3, 4); put(b, b.length, 10); put(b, 1, 20);
    r += sum(b) + b.length;
    var o = { length: 0 }; put(o, 0, 5); put(o, 1, 6); r += o[0] + o[1] + o.length;
    var t = new Int32Array(4); put(t, 0, 3); put(t, 4, 9); r += t[0] + t.length;
    var s = []; put(s, 0, 1); put(s, 1, 2); put(s, "x", 3); r += s.length + s.x;
    var lineMap = [0]; for (var i = 1; i < 100; i++) lineMap[i] = i * 2; r += lineMap[99] + lineMap.length;
  }
  return r;
}
main();"#;
    assert_eq!(value(src), "299980");
    let app = "function app(n) { var a = []; for (var i = 0; i < n; i++) a[a.length] = i; return a; }
var t = 0; for (var k = 0; k < 300; k++) t += app(50).length; t;";
    let (v, s) = compiled(app, "app");
    assert_eq!(v, "15000");
    assert!(s.iter().any(|n| n.starts_with("set_keyed_ap")), "{s:?}");
}

/// An array literal is made with room for its elements, each appended
/// (`create_array_n`, then `push`), whatever their types; past 255
/// elements, it grows.
#[test]
fn array_literals_have_room_for_their_elements() {
    let src = r#"
function f(i) { var a = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]; var b = [i, i * 0.5, "s" + i, {x: i}, [i, [i]]]; var c = []; var d = [i]; return a[9] + b[0] + b[1] + b[2].length + b[3].x + b[4][1][0] + c.length + d[0] + a.length + b.length; }
var t = 0; for (var i = 0; i < 100000; i++) t += f(i);
var big = []; for (var i = 0; i < 300; i++) big.push(i);
var lit300 = eval("[" + big.join(",") + "]");
t + " " + lit300.length + " " + lit300[299] + " " + JSON.stringify([1, [2, 3], "x", null, true]);"#;
    assert_eq!(value(src), "22502763890 300 299 [1,[2,3],\"x\",null,true]");
    let (_, s) = compiled(src, "f");
    assert!(s.iter().any(|n| n == "create_array_n"), "{s:?}");
}

/// A recursive call whose arguments the version making it doesn't take
/// (its entry typed by its callers, the call's argument a field of any
/// type) still calls directly: a version for those types, bound at the
/// first call, not the runtime at each.
#[test]
fn self_calls_bind_versions_for_their_types() {
    let src = r#"
function P(car, cdr) { this.car = car; this.cdr = cdr; }
function T(kids) { this.kids = kids; }
function walk(t, d) {
  if (!(t instanceof T)) return d;
  var n = 0, l = t.kids;
  while (l !== null) { n += walk(l.car, d + 1); l = l.cdr; }
  return n;
}
function tree(k) { if (k == 0) return k; var l = null; for (var i = 0; i < 3; i++) l = new P(tree(k - 1), l); return new T(l); }
var t = tree(6), s = 0;
for (var r = 0; r < 200; r++) s += walk(t, r & 3);
s;"#;
    assert_eq!(value(src), "1093500");
    let (_, s) = compiled(src, "walk");
    assert!(s.iter().any(|n| n.starts_with("call_direct")), "{s:?}");
    assert!(!s.iter().any(|n| n == "call"), "{s:?}");
}

/// Prototype's `Class.create` constructors, one code for every class
/// (`this.initialize.apply(this, arguments)`): inlined at `new`, `this`
/// of the class's root shape, its `initialize` found on the prototype and
/// called with the `new`'s arguments (`arguments` too), and replaced later.
#[test]
fn class_create_constructors_inlined() {
    let src = r#"
var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var Vec = Class.create();
Vec.prototype = {
  initialize: function (x, y, z) { this.x = x ? x : 0; this.y = y ? y : 0; this.z = z ? z : 0; },
  add: function (w) { return new Vec(this.x + w.x, this.y + w.y, this.z + w.z); },
  scale: function (k) { return new Vec(this.x * k, this.y * k, this.z * k); }
};
var Ray = Class.create();
Ray.prototype = { initialize: function (o, d) { this.o = o; this.d = d; this.t = new Vec(1, 2); } };
var Named = Class.create();
Named.prototype = { initialize: function () { this.n = arguments.length; this.first = arguments[0]; } };
function main(n) {
  var acc = new Vec(), s = 0;
  for (var i = 0; i < n; i++) {
    var r = new Ray(new Vec(i, 1, 2), new Vec(0, i & 3));
    acc = acc.add(r.o.scale(0.5)).add(r.d).add(r.t);
    s += new Named(i, 2, 3).n + new Named().n + (new Named(7).first);
  }
  return [acc.x, acc.y, acc.z, s].join(",");
}
var out = main(20000);
Vec.prototype.initialize = function (x, y, z) { this.x = -1; this.y = -1; this.z = -1; };
out + " " + main(3);"#;
    assert_eq!(value(src), "100015000,80000,20000,200000 -1,-1,-1,30");
    let (_, s) = compiled(src, "main");
    // No `new` through the runtime, nor a call (direct ones are).
    assert!(!s.iter().any(|n| n == "call" || n.starts_with("construct")), "{s:?}");
}

/// `eval`'s and `Function`'s functions compile at their first calls,
/// parsed again where they are (see `asm::LazyFn`): upvalues through
/// functions not compiled yet, named function expressions, `arguments`,
/// handlers, constructors. The expected value is Node's.
#[test]
fn eval_functions_compile_when_first_called() {
    let src = r#"
var src = "var k = 10;\n" +
  "function outer(a) { var b = a * 2; function mid(c) { var z = 5; return function inner(d) { return [a, b, c, d, k, typeof z].join(':'); }; } return mid(1); }\n" +
  "function counter() { var n = 0; return { inc: function () { return ++n; }, get: function () { return n; } }; }\n" +
  "function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }\n" +
  "var named = function self(n) { return n ? self(n - 1) + 1 : 0; };\n" +
  "function args() { return arguments.length + ':' + Array.prototype.join.call(arguments, '/'); }\n" +
  "function thrower() { try { (function () { throw new Error('in'); })(); } catch (e) { return 'caught ' + e.message; } }\n" +
  "function shadow(a) { var k = 'local'; return function (k2) { return k + a + k2; }; }\n" +
  "function deep(x) { return function (y) { return function (z) { return function () { return x + y + z; }; }; }; }\n" +
  "function Point(x, y) { this.x = x; this.y = y; }\n" +
  "Point.prototype.norm = function () { return Math.sqrt(this.x * this.x + this.y * this.y); };\n" +
  "function late() { return neverCalled; function neverCalled() { return 1; } }\n" +
  "var c = counter(); c.inc(); c.inc();\n" +
  "[outer(3)(4), c.get(), fact(10), named(5), args(1, 2, 3), thrower(), shadow('A')('B'),\n" +
  " deep(1)(2)(3)(), new Point(3, 4).norm(), typeof late()].join(',');";
var direct = (0, eval)(src);
var made = Function("p", "q", "function sq(v) { return v * v; } return sq(p) + q;")(3, 4);
var bad = (0, eval)("(function () { return 1; })");
direct + "|" + made + "|" + bad();
"#;
    assert_eq!(value(src), "3:6:1:4:10:number,2,3628800,5,3:1/2/3,caught in,localAB,6,5,function|13|1");
    // Bodies only skimmed: the names they may use from outside, a few
    // more than they do (ASI ending a `var`, accessors, keys and labels
    // as outer variables, `catch`'s, hoisting). Node's values.
    let skimmed = r#"
var src = [
  "function t1() { var x = 1; var f = function () { var g = function () { var x = 7; return x; }; return g() + x; }; return f(); }",
  "function t2() { var b = 0; var f = function () { var a = 1\n b = 2 }; f(); return b; }",
  "function t3() { var b = 0; var f = function () { var a = 1, b = 2; return a + b; }; return f() + ':' + b; }",
  "function t4() { var k = 0, o = { p: 1, q: 2 }; var f = function () { var s = ''; for (var k in o) s += k; return s; }; return f() + k; }",
  "function t5() { var y = 5; var f = function () { var o = { get v() { var y = 1; return y; }, set v(w) { y = w; } }; o.v = 9; return o.v + ':' + y; }; return f() + ':' + y; }",
  "function t6() { var k = 3; var f = function () { return { k: 1 }.k + k; }; return f(); }",
  "function t7() { var lbl = 4; var f = function () { lbl: for (;;) { break lbl; } return lbl; }; return f(); }",
  "function t8() { var e = 'outer'; var f = function () { try { throw 'x'; } catch (e) { } return e; }; return f(); }",
  "function t9() { var f = function () { var r = function fib(n) { return n < 2 ? n : fib(n - 1) + fib(n - 2); }; return r(10); }; return f(); }",
  "function t10() { var f = function () { return function () { return arguments.length; }(1, 2, 3); }; return f(4); }",
  "function t11(a) { return function (b) { return function (c) { return function (d) { return [a, b, c, d].join(''); }; }; }; }",
  "function t12() { var x = 1; var f = function () { x = 2; var x; return x; }; return f() + ':' + x; }",
  "function t13() { var n = 3; var f = function () { return h(); function h() { return n * 2; } }; return f(); }",
  "function t14() { var m = 1; var f = function () { var a = [1, 2], m = a.length; return m; }; return f() + ':' + m; }",
  "function t15() { var q = 1; var f = function () { var a = (q, 5); return a + q; }; return f(); }",
  "function t16() { var z = 2; var f = function () { var a = { z: 1, w: function () { return z; } }; return a.w() + a.z; }; return f(); }",
  "function t17() { var c = 0; var f = function () { var a = 1\n b = 2, c = 3 }; f(); return c; }",
  "[t1(), t2(), t3(), t4(), t5(), t6(), t7(), t8(), t9(), t10(), t11(1)(2)(3)(4), t12(), t13(), t14(), t15(), t16(), t17()].join(',');"
].join("\n");
(0, eval)(src);
"#;
    assert_eq!(value(skimmed), "8,2,3:0,pq0,1:9:9,4,4,outer,55,3,1234,2:1,6,2:1,6,3,3");
    // A direct `eval` in a body sees the variables around it, though
    // nothing else there names them: reads, writes (not a global's), in
    // a nested function or an accessor, `(eval)`. SpiderMonkey's values.
    let evals = r#"
var src = [
  "function e1() { var z = 1; return function () { return eval('z'); }; }",
  "function e2() { var z = 1; var f = function () { eval('z = 2'); }; f(); return z + ':' + typeof globalThis.z; }",
  "function e3() { var a = 1; return function () { var b = 2; return function () { return eval('a + b'); }(); }; }",
  "function e4(p) { var o = { get v() { return eval('p'); } }; return function () { return o.v + (eval)('p'); }; }",
  "function e5() { function h() { return 5; } return function () { function k() { return eval('h()'); } return k(); }; }",
  "[e1()(), e2(), e3()(), e4(2)(), e5()()].join(',');"
].join("\n");
(0, eval)(src);
"#;
    assert_eq!(value(evals), "1,2:undefined,3,4,5");
    // A method's `var`s are its own (#94), however it's written; a call
    // and a block after it, or an `if` or a `for`, read as one, still
    // refer to their names. SpiderMonkey's values.
    let methods = r#"
var src = [
  "function m1() { var x = 1; return function () { ({ m() { var x }, *g() { var x }, async a() { var x } }); return x; }; }",
  "function m2() { var x = 2; return function () { ({ 'm'(p, q) { var x }, 1() { var x }, [x]() { var x }, if() { var x } }); return x; }; }",
  "function m3() { var x = 3; return function () { ({ var() { var x }, get() { var x }, m()\n{ var x }, n({ p }) { var x } }); return x; }; }",
  "function m4() { var a = 4; return function () { var f = function () {}; f(a)\n{ var y } l: { if (a) { var z } } return a; }; }",
  "function m5() { var a = [5], x = 0; return function () { l: { for (x of a) { } } return x; }; }",
  "function m6(k) { var a = [6], x = 0; return function () { switch (k) { case 1: { for (x of a) { } } } return x; }; }",
  "function m7() { var a = [7], x = 0; return function () { var t = 1\n{ for (x of a) { } } return x; }; }",
  "[m1()(), m2()(), m3()(), m4()(), m5()(), m6(1)(), m7()()].join(',');"
].join("\n");
(0, eval)(src);
"#;
    assert_eq!(value(methods), "1,2,3,4,5,6,7");
    // A skimmed body's syntax error is thrown at its first call (Node
    // throws at the `eval`).
    assert_eq!(
        value(
            "var f = (0, eval)('var g = function () { return 1 + ; }; g'); \
               var r; try { f(); } catch (e) { r = String(e).indexOf('SyntaxError') >= 0; } r;"
        ),
        "true"
    );
    // What only the code generator rejects is thrown at the call.
    assert_eq!(
        value(
            "var f = (0, eval)('(function () { return function () { while (1) { continue x; } }; })')(); \
               var r; try { f(); } catch (e) { r = String(e).indexOf('no target') >= 0; } r;"
        ),
        "true"
    );
}

/// Every running microbenchmark through `eval`, so each of its functions
/// is compiled lazily, computes what it does compiled up front.
#[test]
fn microbenchmarks_through_eval_compute_their_expected_results() {
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if !RUNNING.contains(&number(name)) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        let expect = src.lines().find_map(|l| l.strip_prefix("// expect: ")).unwrap();
        let mut quoted = String::from("(0, eval)(\"");
        for c in src.chars() {
            match c {
                '\\' => quoted.push_str("\\\\"),
                '"' => quoted.push_str("\\\""),
                '\n' => quoted.push_str("\\n"),
                '\r' => quoted.push_str("\\r"),
                '\u{2028}' => quoted.push_str("\\u2028"),
                '\u{2029}' => quoted.push_str("\\u2029"),
                c => quoted.push(c),
            }
        }
        quoted.push_str("\");");
        assert_eq!(value(&quoted), expect.trim(), "{name}");
    }
}

/// Promise jobs run once the script is done, in the order queued, the
/// ones they queue after them (what node prints for the same script); a
/// later script in the VM sees what they did.
#[test]
fn promise_jobs_run_after_the_script() {
    let first = cloison::js::compile(
        r#"
var log = [];
Promise.resolve(1).then(function (v) { log.push("a" + v); });
Promise.reject(new Error("x")).catch(function (e) { log.push("b" + e.message); return 7; })
  .then(function (v) { log.push("c" + v); });
var p = new Promise(function (res, rej) { log.push("exec"); res(3); rej(4); res(5); });
p.then(function (v) { log.push("d" + v); throw "boom"; })
  .then(null, function (r) { log.push("e" + r); })
  .finally(function () { log.push("f"); });
Promise.resolve({ then: function (ok) { log.push("thenable"); ok(9); } })
  .then(function (v) { log.push("g" + v); });
Promise.all([1, Promise.resolve(2), { then: function (ok) { ok(3); } }])
  .then(function (vs) { log.push("all" + vs.join(",")); });
Promise.race([new Promise(function () {}), Promise.resolve("r")])
  .then(function (v) { log.push("race" + v); });
Promise.allSettled([Promise.reject("no"), 1])
  .then(function (rs) { log.push("settled" + rs[0].status + rs[0].reason + rs[1].status + rs[1].value); });
Promise.any([Promise.reject(1), Promise.reject(2)])
  .catch(function (e) { log.push("any" + e.constructor.name + e.errors.join("/") + ":" + (e instanceof Error)); });
var q = Promise.resolve();
q.then(function () { log.push("1"); q.then(function () { log.push("3"); }); });
q.then(function () { log.push("2"); });
var self = new Promise(function (r) { setSelf = r; });
setSelf(self);
self.catch(function (e) { log.push("self" + (e instanceof TypeError)); });
Promise.resolve(1).then(2, 3).then(function (v) { log.push("passthru" + v); });
var rf; new Promise(function (res, rej) { rf = rej; });
log.push(rf.name === "" && rf.length === 1 && !rf.hasOwnProperty("prototype"));
log.push("sync");
"#,
    )
    .unwrap();
    let second = cloison::js::compile("log.join(' ')").unwrap();
    for budget in [i64::MAX, 1, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run(&first).unwrap();
        let r = vm.run(&second).map(display).unwrap();
        vm.verify_heap();
        assert_eq!(
            r,
            "exec true sync a1 bx d3 thenable 1 2 selftrue c7 eboom g9 racer \
             settledrejectednofulfilled1 anyAggregateError1/2:true 3 passthru1 f all1,2,3",
            "tier at {budget}"
        );
    }
}

/// Code that eval'd functions no longer reachable leave is freed (by a
/// full collection, which code loaded starts), while the functions kept,
/// and those nested in them, run on: of 3000 evals' 12,000 functions, in
/// the interpreter, under 2000 stay. (Compiled, the call `o.f()` binds
/// each callee, which keeps its code: see `vm::Code::pinned`.)
#[test]
fn dead_eval_code_is_freed() {
    let src = r#"
var keep = [];
function make(i) {
  return (0, eval)("(function () { var k = " + i + "; function inner() { return k * 2; } " +
                   "return { f: function () { return inner() + k; } }; })()");
}
var sum = 0;
for (var i = 0; i < 3000; i++) {
  var o = make(i);
  sum += o.f();
  if (i % 100 == 0) keep.push(o);
  var junk = [];
  for (var j = 0; j < 400; j++) junk.push({ a: j });
}
for (var i = 0; i < keep.length; i++) sum += keep[i].f();
sum;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_code_room(64 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r, Ok("13626000".to_string()), "tier at {budget}");
        vm.verify_heap();
        if budget == i64::MAX {
            assert!(vm.code_count() < 2000, "{} codes", vm.code_count());
        }
    }
}

/// Arrays past `heap::LARGE` (here 3968 typed arrays, 32 KB of elements)
/// are cells of their own blocks: one in a small block left the young
/// values stored more than 8 KB into it, behind cards a minor collection
/// couldn't find the cell for, to be freed (`Heap::alloc`). Checked with
/// collections every 64 KB and freed memory poisoned.
#[test]
fn young_values_far_into_big_arrays_survive() {
    let src = r#"
function make() { return new Uint8Array(64); }
var keep = [];
for (var r = 0; r < 12; r++) {
  var a = [];
  var i = 0;
  while (i < 0xF80) { a[i++] = make(); }
  keep.push(a);
}
var ok = 0;
for (var r = 0; r < keep.length; r++)
  for (var i = 0; i < keep[r].length; i++) if (keep[r][i].length == 64) ok++;
ok;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_gc_verify(64 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r, Ok("47616".to_string()), "tier at {budget}");
        vm.verify_heap();
    }
}

/// Async generators (awaits, queued requests, `return`/`throw`, `finally`)
/// and `for await` over async, sync and custom iterators, with `yield*`.
#[test]
fn async_generators_and_for_await() {
    for (src, expected) in [
        (
            r##"
var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
async function* gen(n) {
  for (let i = 0; i < n; i++) { await null; yield i; }
  return "end";
}
async function* thrower() { yield 1; throw new Error("boom"); }
async function* awaitsYield() { var x = yield Promise.resolve("p"); log("got", x); yield x * 2; }
async function* withFinally() { try { yield 1; yield 2; } finally { log("cleanup"); } }
var syncIterable = { [Symbol.iterator]() { var i = 0; return { next() { i++; return { value: i < 4 ? Promise.resolve(i * 10) : undefined, done: i >= 4 }; }, return() { log("sync return"); return {}; } }; } };
var customAsync = { [Symbol.asyncIterator]() { var i = 0; return { next() { i++; return Promise.resolve({ value: "c" + i, done: i > 2 }); }, return() { log("async return"); return Promise.resolve({ done: true }); } }; } };
(async () => {
  var it = gen(3);
  log(typeof it.next, Object.prototype.toString.call(it), typeof it[Symbol.asyncIterator], it[Symbol.asyncIterator]() === it);
  var r1 = it.next(), r2 = it.next();
  log(r1 instanceof Promise, JSON.stringify(await r1), JSON.stringify(await r2));
  log(JSON.stringify(await it.next()), JSON.stringify(await it.next()), JSON.stringify(await it.next()));
  var vals = []; for await (var v of gen(4)) vals.push(v); log("for await gen", vals.join());
  var t = thrower(); log(JSON.stringify(await t.next()));
  try { await t.next(); } catch (e) { log("caught", e.message); }
  log(JSON.stringify(await t.next()));
  var ay = awaitsYield(); log(JSON.stringify(await ay.next())); log(JSON.stringify(await ay.next(21))); log(JSON.stringify(await ay.next()));
  var wf = withFinally(); log(JSON.stringify(await wf.next())); log(JSON.stringify(await wf.return("r"))); log(JSON.stringify(await wf.next()));
  for await (var w of withFinally()) { log("w", w); break; }
  var sv = []; for await (const s of syncIterable) sv.push(s); log("sync", sv.join());
  for await (const s of syncIterable) { log("s", s); break; }
  var cv = []; for await (let c of customAsync) cv.push(c); log("custom", cv.join());
  for await (let c of customAsync) { break; }
  var arr = []; for await (const x of [1, Promise.resolve(2), 3]) arr.push(x); log("array", arr.join());
  try { for await (const x of gen(5)) { if (x == 1) throw new Error("inner"); } } catch (e) { log("loop throw", e.message); }
  var early = gen(2); var ret = await early.return(7); log("early return", JSON.stringify(ret), JSON.stringify(await early.next()));
  var ethrow = gen(2); try { await ethrow.throw(new Error("et")); } catch (e) { log("early throw", e.message); }
  async function* inf() { var i = 0; while (true) yield i++; }
  var s = 0; for await (const x of inf()) { if (x > 100) break; s += x; } log("inf", s);
  var queued = gen(3); var ps = [queued.next(), queued.next(), queued.next(), queued.next()]; log("queued", (await Promise.all(ps)).map(r => r.value + "/" + r.done).join());
  class K { async *m() { yield this.v; } constructor() { this.v = "method"; } }
  for await (const x of new K().m()) log(x);
  var o = { async *[Symbol.asyncIterator]() { yield "obj"; } }; for await (const x of o) log(x);
  var ae = async function* () { yield await new Promise(r => r("x")); };
})().catch(e => log("ERR", e));"##,
            "function [object AsyncGenerator] function true | true {\"value\":0,\"done\":false} {\"value\":1,\"done\":false} | {\"value\":2,\"done\":false} {\"value\":\"end\",\"done\":true} {\"done\":true} | for await gen 0,1,2,3 | {\"value\":1,\"done\":false} | caught boom | {\"done\":true} | {\"value\":\"p\",\"done\":false} | got 21 | {\"value\":42,\"done\":false} | {\"done\":true} | {\"value\":1,\"done\":false} | cleanup | {\"value\":\"r\",\"done\":true} | {\"done\":true} | w 1 | cleanup | sync 10,20,30 | s 10 | sync return | custom c1,c2 | async return | array 1,2,3 | loop throw inner | early return {\"value\":7,\"done\":true} {\"done\":true} | early throw et | inf 5050 | queued 0/false,1/false,2/false,end/true | method | obj",
        ),
        (
            r##"
var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
async function* inner() { var x = yield 1; log("inner got", x); try { yield 2; } finally { log("inner finally"); } return "inner-done"; }
async function* outer() { var r = yield* inner(); log("delegate result", r); yield 3; }
async function* overSync() { var r = yield* [Promise.resolve("a"), "b"]; log("sync r", r); }
async function* overThrow() { try { yield* inner(); } catch (e) { log("outer caught", e); yield "recovered"; } }
async function* plain() { yield* (async function* () { yield 10; yield 20; })(); }
(async () => {
  var it = outer();
  log(JSON.stringify(await it.next()), JSON.stringify(await it.next("X")), JSON.stringify(await it.next()), JSON.stringify(await it.next()), JSON.stringify(await it.next()));
  var vs = []; for await (const v of overSync()) vs.push(v); log("over sync", vs.join());
  var t = overThrow(); await t.next(); await t.next(); log(JSON.stringify(await t.throw("E")));
  var r = outer(); await r.next(); log(JSON.stringify(await r.return("early")), JSON.stringify(await r.next()));
  var p = []; for await (const v of plain()) p.push(v); log("plain", p.join());
})().catch(e => log("ERR", e));"##,
            "inner got X | inner finally | delegate result inner-done | {\"value\":1,\"done\":false} {\"value\":2,\"done\":false} {\"value\":3,\"done\":false} {\"done\":true} {\"done\":true} | sync r undefined | over sync a,b | inner got undefined | inner finally | outer caught E | {\"value\":\"recovered\",\"done\":false} | {\"value\":\"early\",\"done\":true} {\"done\":true} | plain 10,20",
        ),
    ] {
        let first = cloison::js::compile(src).unwrap();
        let second = cloison::js::compile("out.join(' | ')").unwrap();
        for budget in [i64::MAX, 1, 1000] {
            let mut vm = Vm::new();
            vm.set_tier_back_edges(budget);
            vm.run(&first).unwrap();
            let r = vm.run(&second).map(display).unwrap();
            assert_eq!(r, expected, "tier at {budget}");
        }
    }
}

/// A name declared twice where one is block-scoped is an early error;
/// what may share a name (`var`s, a catch parameter's `var`, sloppy
/// block functions, the same name in sibling blocks) still compiles.
#[test]
fn redeclarations_are_early_errors() {
    for src in [
        "let x; var x;",
        "var x; let x;",
        "let x; let x;",
        "const x = 1; let x;",
        "{ let y; var y; }",
        "{ var z; } let z;",
        "function f(a) { let a; }",
        "function f({ a }) { const a = 1; }",
        "'use strict'; { function g() {} function g() {} }",
        "{ function h() {} var h; }",
        "switch (1) { case 1: let s; case 2: let s; }",
        "for (let i;;) { var i; }",
        "class C {} let C;",
        "function u() { let w; { var w; } }",
    ] {
        assert!(compile_error(src).contains("has already been declared"), "{src}");
    }
    let src = r#"
for (let i = 0; i < 1; i++) {} for (let i = 0; i < 1; i++) {}
{ let x = 1; } { let x = 2; }
function f(a) { var a; function a() {} return typeof a; }
function g(a) { { let a = 1; } }
var dup; var dup;
function h() {} function h() {}
try {} catch (e) { var e = 1; }
{ function k() {} function k() {} }
let outer = 1; function inner() { let outer = 2; var y; return outer; }
f(1) + inner();"#;
    assert_eq!(value(src), "function2");
}

/// An indexed restriction on an unrelated object cannot affect any other
/// receiver's stores. Once such an object becomes a prototype, its existing
/// indexed properties must still protect those stores (#240).
#[test]
fn indexed_store_protector_is_scoped_to_prototypes() {
    let src = r#"
Object.freeze([1]);
function fill(n) { var a = [], s = 0; for (var i = 0; i < n; i++) { a[i] = i; s += a[i]; } return s; }
var out = 0;
for (var k = 0; k < 100; k++) out += fill(100);
out;
"#;
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(20);
    assert_eq!(display(vm.run(&m).unwrap()), "495000");
    let st = vm.jit_stats("<script>").expect("script compiled");
    let stencils = vm.jit_stencils("<script>").unwrap();
    assert!(stencils.iter().any(|s| s.starts_with("set_keyed_")), "{stencils:?}");
    assert!(st.exits <= 1, "{st:?}");

    assert_eq!(
        value(
            r#"
var p = Object.freeze([1]);
function C() {} C.prototype = p;
var o = new C(); o[0] = 7;
[o[0], o.hasOwnProperty(0)].join();
"#,
        ),
        "1,false",
    );
}

/// A typed array's element at a double key (an index plus a
/// `Uint32Array`'s element, pdf.js's `output[index + xs[x]]`) is loaded in
/// compiled code, not through the runtime; a key naming no index (a
/// fraction, a negative, 2^32 - 1, NaN) or past the end is `undefined`,
/// and `-0` is 0 (#240: once keyed stores compiled, pdf.js ran 50% more
/// instructions through the runtime's keyed loads).
#[test]
fn typed_arrays_load_at_double_keys() {
    let src = r#"
var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
var out = [];
for (var K of kinds) {
  var sum = new Function("a", "idx", "off", "var s = 0; for (var x = 0; x < idx.length; x++) { var v = a[off + idx[x]]; s += v === undefined ? 1000 : v; } return s;");
  var a = new K(16); for (var i = 0; i < 16; i++) a[i] = i * 3 - 7;
  var idx = new Uint32Array([0, 1, 2, 3, 4, 5, 6, 7]);
  var s = 0; for (var r = 0; r < 3000; r++) s += sum(a, idx, 0.5 - 0.5);
  var keys = [0.5 - 0.5, -0, 1.5, -1, 15, 16, 2 ** 32 - 1, 2 ** 32, NaN, Infinity, 3];
  var got = keys.map(function (k) { var t = 0; for (var r = 0; r < 50; r++) t = sum(a, [0], k); return t; });
  out.push(K.name + " " + s + " " + got.join(","));
}
out.join("\n");
"#;
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    let got = display(vm.run(&m).unwrap());
    let tail = "1000,1000,38,1000,1000,1000,1000,1000,2";
    assert_eq!(
        got,
        [
            format!("Int8Array 84000 -7,-7,{tail}"),
            format!("Uint8Array 2388000 249,249,{tail}"),
            format!("Uint8ClampedArray 120000 0,0,{tail}"),
            format!("Int16Array 84000 -7,-7,{tail}"),
            format!("Uint16Array 589908000 65529,65529,{tail}"),
            format!("Int32Array 84000 -7,-7,{tail}"),
            format!("Uint32Array 38654705748000 4294967289,4294967289,{tail}"),
            format!("Float32Array 84000 -7,-7,{tail}"),
            format!("Float64Array 84000 -7,-7,{tail}"),
        ]
        .join("\n")
    );

    let src = r#"
function fksum(a, idx, off) { var s = 0; for (var x = 0; x < idx.length; x++) s += a[off + idx[x]]; return s; }
var a = new Int16Array(64), idx = new Uint32Array(32), t = 0;
for (var i = 0; i < 64; i++) a[i] = i;
for (var i = 0; i < 32; i++) idx[i] = i;
for (var r = 0; r < 3000; r++) t += fksum(a, idx, r & 7);
t;
"#;
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    assert_eq!(display(vm.run(&m).unwrap()), "1824000");
    let stencils = vm.jit_stencils("fksum").expect("fksum compiled");
    assert!(stencils.iter().any(|s| s.starts_with("tget_i16_fk")), "{stencils:?}");
    assert!(!stencils.iter().any(|s| s.starts_with("get_keyed_rt")), "{stencils:?}");
}

/// Once a fixed-length buffer has been detached (`Vm::views_detached`),
/// typed arrays' elements stay on the stencils, which check the view's
/// buffer (`tget_*_d*`, `tset_*_d*`): a detached view's stale length lets
/// nothing through, an immutable one takes no store, and resizable and
/// out-of-bounds views keep their lengths (#240: pdf.js, which detaches
/// a buffer, ran its typed array loops through the runtime).
#[test]
fn typed_arrays_stay_compiled_once_a_buffer_is_detached() {
    let src = r#"
// detached and immutable views through compiled typed loads and stores
var out = [];
var b0 = new ArrayBuffer(4); b0.transfer(); // a detach: views_detached
function rd(a, n) { var s = 0; for (var i = 0; i < n; i++) { var v = a[i]; s += v === undefined ? 1000 : v; } return s; }
function wr(a, n, v) { for (var i = 0; i < n; i++) a[i] = v + i; return a[0]; }
function rdf(a, idx) { var s = 0; for (var i = 0; i < idx.length; i++) { var v = a[idx[i] + 0.5 - 0.5]; s += v === undefined ? 1000 : v; } return s; }
var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
for (var K of kinds) {
  var a = new K(8);
  for (var r = 0; r < 2000; r++) { wr(a, 8, r & 15); rd(a, 8); rdf(a, new Uint32Array([1, 2, 3])); }
  var buf = a.buffer, s1 = rd(a, 8), f1 = rdf(a, new Uint32Array([1, 2, 3]));
  var c = buf.transfer(); // a's buffer detached
  var s2 = rd(a, 8), f2 = rdf(a, new Uint32Array([1, 2, 3]));
  var w = wr(a, 8, 5);
  var n = new K(c); var s3 = rd(n, 8);
  out.push(K.name + " " + [s1, f1, s2, f2, w, a.length, s3].join(","));
}
// immutable views
if (ArrayBuffer.prototype.transferToImmutable) {
  var im = new Uint8Array(new Uint8Array([1, 2, 3, 4]).buffer.transferToImmutable());
  var m = new Uint8Array(4);
  for (var r = 0; r < 2000; r++) wr(m, 4, r & 7);
  var e = 0; try { wr(im, 4, 9); } catch (x) { e = x.constructor.name; }
  (function () { "use strict"; try { wr(im, 4, 9); } catch (x) { e += "/" + x.constructor.name; } })();
  out.push("imm " + Array.from(im).join() + " " + rd(im, 4) + " " + e);
}
// resizable, length-tracking and out-of-bounds views
var rb = new ArrayBuffer(8, { maxByteLength: 16 });
var tr = new Uint8Array(rb), fx = new Uint8Array(rb, 4, 4);
for (var r = 0; r < 2000; r++) { wr(tr, tr.length, 1); rd(fx, 4); }
rb.resize(6);
out.push("rs " + [rd(tr, 8), rd(fx, 4), fx.length, tr.length, wr(fx, 4, 3), rd(fx, 4)].join());
rb.resize(16);
out.push("rs2 " + [rd(tr, 16), rd(fx, 4), fx.length, tr.length].join());
out.join("\n");
"#;
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    assert_eq!(
        display(vm.run(&m).unwrap()),
        "Int8Array 148,51,8000,3000,,0,148\n\
         Uint8Array 148,51,8000,3000,,0,148\n\
         Uint8ClampedArray 148,51,8000,3000,,0,148\n\
         Int16Array 148,51,8000,3000,,0,148\n\
         Uint16Array 148,51,8000,3000,,0,148\n\
         Int32Array 148,51,8000,3000,,0,148\n\
         Uint32Array 148,51,8000,3000,,0,148\n\
         Float32Array 148,51,8000,3000,,0,148\n\
         Float64Array 148,51,8000,3000,,0,148\n\
         imm 1,2,3,4 10 0\n\
         rs 2021,4000,0,6,,4000\n\
         rs2 21,11,4,16"
    );
    let src = r#"
new ArrayBuffer(4).transfer();
function rdu(a, n) { var s = 0; for (var i = 0; i < n; i++) { var v = a[i]; s += v === undefined ? 1000 : v; } return s; }
function wru(a, n) { for (var i = 0; i < n; i++) a[i] = i; }
var a = new Uint16Array(8), s = 0;
for (var r = 0; r < 3000; r++) { wru(a, 8); s += rdu(a, 8); }
a.buffer.transfer(); wru(a, 8);
s + " " + rdu(a, 8);
"#;
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    assert_eq!(display(vm.run(&m).unwrap()), "84000 8000");
    for f in ["rdu", "wru"] {
        let stencils = vm.jit_stencils(f).expect("compiled");
        assert!(
            stencils.iter().any(|s| s.starts_with("tget_u16_d") || s.starts_with("tset_b16_i_d")),
            "{f}: {stencils:?}"
        );
    }
    // Lengths: 0 once the buffer is detached, a resizable buffer's views'
    // as it resizes.
    let src = r#"
new ArrayBuffer(4).transfer();
function len(a) { var s = 0; for (var r = 0; r < 10; r++) s += a.length; return s; }
function lenAny(a) { var s = 0; for (var r = 0; r < 10; r++) s += a.length; return s; }
var t = new Int32Array(5), arr = [1, 2], str = "abc";
var x = 0; for (var i = 0; i < 3000; i++) { x += len(t); x += lenAny(i % 3 == 0 ? t : i % 3 == 1 ? arr : str); }
t.buffer.transfer();
var rb = new ArrayBuffer(8, { maxByteLength: 16 }), tr = new Uint8Array(rb), fx = new Uint8Array(rb, 4, 4);
for (var i = 0; i < 3000; i++) { lenAny(tr); lenAny(fx); }
rb.resize(6);
[x, len(t), lenAny(t), lenAny(arr), lenAny(tr), lenAny(fx)].join();
"#;
    let m = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    assert_eq!(display(vm.run(&m).unwrap()), "250000,0,0,20,60,0");
    let stencils = vm.jit_stencils("len").expect("len compiled");
    assert!(stencils.iter().any(|s| s.starts_with("get_length_td")), "{stencils:?}");
}

/// Early errors: `yield`/`await` (escaped) as identifiers where they are
/// keywords, `"use strict"` in a function with non-simple parameters; and
/// destructuring defaults name anonymous functions.
#[test]
fn identifier_early_errors_and_default_names() {
    for src in [
        "var g = async function *g() { var yi\\u0065ld; };",
        "async () => { var \\u0061wait; }",
        "0, (a, ...rest) => { 'use strict'; };",
        "function f(a = 1) { 'use strict'; }",
        "function f({ a }) { 'use strict'; }",
        "function* g() { yield: 1; }",
        "'use strict'; var yield;",
    ] {
        compile_error(src);
    }
    let src = "var [x = function () {}] = []; var { y = () => 1 } = {}; var z; ({ z = function () {} } = {}); function f([w = class {}]) { return w.name; } [x.name, y.name, z.name, f([])].join();";
    assert_eq!(value(src), "x,y,z,w");
}

/// A statement's body is no declaration (but a sloppy plain function's),
/// and non-simple parameters, an arrow's and a method's have no duplicate
/// names.
#[test]
fn declaration_bodies_and_duplicate_parameters_are_errors() {
    for src in [
        "if (1) let x = 1;",
        "if (1) const x = 1;",
        "while (0) class C {}",
        "if (1) function* g() {}",
        "l: async function f() {}",
        "'use strict'; if (1) function f() {}",
        "function f(a, a = 1) {}",
        "function f(a, ...a) {}",
        "function f({ a }, a) {}",
        "(a, a) => 1",
        "({ a }, [a]) => 1",
        "x = { m(a, a) {} }",
    ] {
        compile_error(src);
    }
    let src = "if (1) function f() { return 1; } l: function g() { return 2; } function h(a, a) { return a; } f() + g() + h(1, 3);";
    assert_eq!(value(src), "6");
}

/// What an async function's parameters throw rejects its promise (a
/// generator's still throws from the call).
#[test]
fn async_functions_reject_for_their_parameters() {
    let first = cloison::js::compile(
        r#"
var out = [];
async function f(a = (() => { throw new Error("param"); })()) { out.push("body"); }
var p = f(); out.push(p instanceof Promise ? "promise" : "no");
p.catch(e => out.push("rejected " + e.message));
async function g(x = 1, y = x + 1) { return x + y; }
g().then(v => out.push("g " + v));
function* h(a = (() => { throw new Error("gen"); })()) {}
try { h(); } catch (e) { out.push("sync " + e.message); }
async function k({ a }) { return a; }
k().catch(e => out.push("destructure " + e.name));
"#,
    )
    .unwrap();
    let second = cloison::js::compile("out.join(' | ')").unwrap();
    let mut vm = Vm::new();
    vm.run(&first).unwrap();
    assert_eq!(
        vm.run(&second).map(display).unwrap(),
        "promise | sync gen | rejected param | g 3 | destructure TypeError"
    );
}

/// A `WeakMap`'s and a `WeakSet`'s entries go once their keys are
/// unreachable (values reachable only from them too, ephemeron chains
/// followed); live keys keep theirs.
#[test]
fn weak_collections_drop_unreachable_keys() {
    let src = r#"
var wm = new WeakMap(), ws = new WeakSet(), keep = [];
for (var i = 0; i < 100000; i++) {
  var k = { i: i };
  if (i % 1000 == 0) keep.push(k);
  wm.set(k, { v: i, big: new Array(8).fill(i) });
  ws.add(k);
}
var head = { n: 0 }, cur = head;
for (var j = 1; j < 50; j++) { var next = { n: j }; wm.set(cur, { next: next }); cur = next; }
wm.set(cur, { end: true });
for (var r = 0; r < 200; r++) { var a = []; for (var q = 0; q < 2000; q++) a.push({ q: q }); }
var ok = keep.every(function (k) { return wm.get(k).v === k.i && wm.get(k).big[7] === k.i && ws.has(k); });
var steps = 0, c = head;
while (wm.has(c) && !wm.get(c).end) { c = wm.get(c).next; steps++; }
ok + ":" + steps;
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r, Ok("true:49".to_string()));
    vm.verify_heap();
    assert!(vm.weak_entry_count() < 100000, "{} weak entries", vm.weak_entry_count());
}

/// A weak collection reached only through another's value, a `WeakMap`
/// or a resizable buffer's set of views, becomes live while the ephemerons
/// are followed: its live keys keep their values too, and its dead keys go.
#[test]
fn weak_collections_reached_through_weak_collections() {
    let src = r#"
var outer = new WeakMap(), key = {}, k2 = {};
function setup() {
  var vals = [];
  for (var j = 0; j < 4000; j++) vals.push({ payload: "mine" });
  var inner = new WeakMap();
  inner.set(k2, vals[2000]);
  outer.set(key, inner);
  outer.set(k2, new ArrayBuffer(16, { maxByteLength: 64 }));
}
setup();
var junk = [];
for (var i = 0; i < 1000000; i++) junk[i & 4095] = { payload: "junk" };
function views() { var b = outer.get(k2); for (var j = 0; j < 4000; j++) new Uint8Array(b); }
views();
for (var i = 0; i < 1000000; i++) junk[i & 4095] = [i, i, i];
outer.get(k2).resize(32);
var bad = 0;
for (var i = 0; i < 4096; i++) if (junk[i].length !== 3) bad++;
outer.get(key).get(k2).payload + " " + bad;
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r, Ok("mine 0".to_string()));
    vm.verify_heap();
}

/// A `WeakRef`'s target stays through the job that made (or dereferenced)
/// it, and goes once unreachable after; brand checks and argument errors
/// as the spec has them.
#[test]
fn weak_refs() {
    let first = cloison::js::compile(
        r#"
var out = [];
function t(n, f) { try { out.push(n + "=" + f()); } catch (e) { out.push(n + "=!" + e.name); } }
var keep = { k: 1 }, rk = new WeakRef(keep), refs = [];
t("tag", () => Object.prototype.toString.call(rk));
t("noNew", () => WeakRef({}));
t("prim", () => new WeakRef(1));
t("regSym", () => new WeakRef(Symbol.for("x")));
t("sym", () => typeof new WeakRef(Symbol("y")).deref());
t("brand", () => WeakRef.prototype.deref.call({}));
t("wsBrand", () => WeakSet.prototype.has.call(rk, keep));
for (var i = 0; i < 1000; i++) refs.push(new WeakRef({ i: i }));
t("sameJob", () => refs.filter(x => x.deref() === undefined).length);
Promise.resolve().then(() => {
  for (var r = 0; r < 300; r++) { var a = []; for (var q = 0; q < 2000; q++) a.push({ q: q }); }
  t("later", () => refs.filter(x => x.deref() === undefined).length > 500);
  t("kept", () => rk.deref() === keep);
});
"#,
    )
    .unwrap();
    let second = cloison::js::compile("out.join(' ')").unwrap();
    let mut vm = Vm::new();
    vm.run(&first).unwrap();
    assert_eq!(
        vm.run(&second).map(display).unwrap(),
        "tag=[object WeakRef] noNew=!TypeError prim=!TypeError regSym=!TypeError sym=symbol brand=!TypeError \
         wsBrand=!TypeError sameJob=0 later=true kept=true"
    );
}

/// The cells compiled code embeds go with it: a loop compiled again for
/// each closure it calls (each of its own code, inlined, its array a
/// constant upvalue) keeps the arrays of the versions it dropped no more
/// (all but the last 4 compiled; with every cell compiled code ever
/// embedded kept for good, only 4 went). And the converse: a compiled loop
/// still calls the closure it checks for, and only that one, after the
/// program drops it and makes others, collections in between.
#[test]
fn compiled_code_keeps_its_cells_while_it_lives() {
    let first = cloison::js::compile(
        r#"
var refs = [], last;
function make(k) {
  var p = (0, eval)("(function () { var o = [" + k + "]; " +
    "return [o, function (i) { return o[0] + i; }]; })()");
  refs.push(new WeakRef(p[0]));
  return p[1];
}
function run() {
  var sum = 0;
  for (var k = 0; k < 40; k++) {
    var f = make(k);
    for (var i = 0; i < 3000; i++) sum += f(i);
    last = f;
  }
  return sum;
}
var s = run(), out = [];
function loop(g, n) { var t = 0; for (var i = 0; i < n; i++) t += g(i); return t; }
function mk(n) { return (0, eval)("(function (i) { return i + " + n + "; })"); }
var a = mk(1000);
out.push(loop(a, 20000));
a = null;
Promise.resolve().then(() => {
  $262.gc();
  $262.gc();
  var dead = refs.filter(r => r.deref() === undefined).length;
  out.push(s, dead >= 30, refs[39].deref()[0], last(1));
  var ok = true;
  for (var k = 0; k < 200; k++) {
    var b = mk(k);
    if (loop(b, 100) !== 4950 + 100 * k) ok = false;
    if (k % 50 == 0) $262.gc();
  }
  out.push(ok);
});
"#,
    )
    .unwrap();
    let second = cloison::js::compile("out.join(' ')").unwrap();
    let mut vm = Vm::new();
    vm.run(&first).unwrap();
    assert_eq!(vm.run(&second).map(display).unwrap(), "219990000 182280000 true 39 40 true");
}

/// A function the JIT took (compiled, its enclosing one pinned to fold
/// its variables) that can't run again, no closure of it left, keeps
/// neither its compiled code nor what that embeds: eval'd functions, each
/// compiled for a loop over an array it closes over, then dropped, leave
/// their arrays to be collected (none went: their codes are never freed,
/// and were live for good).
#[test]
fn compiled_functions_no_longer_reachable_drop_their_code() {
    let first = cloison::js::compile(
        r#"
var refs = [], sum = 0, out = [];
for (var k = 0; k < 30; k++) {
  var p = (0, eval)("(function () { var big = [" + k + ", 1, 2]; " +
    "return [big, function (n) { var s = 0; for (var i = 0; i < n; i++) s += big[i % 3]; return s; }]; })()");
  refs.push(new WeakRef(p[0]));
  sum += p[1](20000);
  p = null;
}
Promise.resolve().then(() => {
  $262.gc();
  $262.gc();
  out.push(sum, refs.filter(r => r.deref() === undefined).length >= 25);
});
"#,
    )
    .unwrap();
    let second = cloison::js::compile("out.join(' ')").unwrap();
    let mut vm = Vm::new();
    vm.run(&first).unwrap();
    assert_eq!(vm.run(&second).map(display).unwrap(), "3500115 true");
}

/// Lengths of 2^31 (a byte view of a whole 2 GiB buffer, its bytes never
/// touched, so never committed) are numbers, not int32s wrapped negative:
/// a buffer's `byteLength`, a view's `length`, `byteLength` and
/// `byteOffset`, and compiled code's `length`, at a site compiled before
/// such a view was made (its shape known) and at one after; an
/// `ArrayBuffer`'s, then a `SharedArrayBuffer`'s, then a view one byte
/// shorter, which compiled code reads as an int32 (#226). One test, its
/// scripts one after the other: each VM reserves 2 GiB, freed with it.
#[cfg(target_pointer_width = "64")]
#[test]
fn lengths_past_i32_max_stay_positive() {
    let src = r#"
function g(u) { var n = 0; for (var i = 0; i < 2000; i++) { u[0] = 1; n = u.length; } return n; }
function f(x) { return x.length; }
var small = new Uint8Array(8), r = 0;
for (var j = 0; j < 10; j++) { r += g(small); for (var i = 0; i < 2000; i++) r += f(small); }
var a = new ArrayBuffer(2 ** 31), t = new Int8Array(a), e = new Uint8Array(a, 2 ** 31);
var q = 0;
for (var i = 0; i < 2000; i++) q = f(t) + 1;
[r, a.byteLength, a.maxByteLength, t.length, t.byteLength, g(t), f(t), q, t[2 ** 31 - 1],
 e.byteOffset, new DataView(a).byteLength, new DataView(a, 2 ** 31).byteOffset, g(small)].join();"#;
    let n = "2147483648";
    assert_eq!(value(src), format!("160080,{n},{n},{n},{n},{n},{n},2147483649,0,{n},{n},{n},8"));
    let src = r#"
var s = new SharedArrayBuffer(2 ** 31), t = new Int8Array(s);
function f(x) { return x.length; }
var r = 0;
for (var i = 0; i < 2000; i++) r = f(t);
[s.byteLength, s.maxByteLength, t.length, t.byteLength, r, new DataView(s).byteLength].join();"#;
    assert_eq!(value(src), [n; 6].join(","));
    let src = r#"
var t = new Uint8Array(new ArrayBuffer(2 ** 31), 1);
function g(u) { var n = 0; for (var i = 0; i < 2000; i++) n = u.length; return n; }
function f(x) { return x.length; }
var r = 0, q = 0;
for (var i = 0; i < 2000; i++) { r = f(t); q = f(t) + 1; }
[t.length, t.byteOffset, r, q, g(t)].join();"#;
    assert_eq!(value(src), "2147483647,1,2147483647,2147483648,2147483647");
}

/// `Array.fromAsync` over sync and async iterables and array-likes, with
/// a mapper, a constructor `this`, and an iterator closed when the mapper
/// throws.
#[test]
fn array_from_async() {
    let src = r#"
var log = [];
async function* ag() { yield 1; yield Promise.resolve(2); }
var closed = 0;
var it = { [Symbol.iterator]() { var i = 0; return { next() { return { done: ++i > 2, value: i }; }, return() { closed++; return {}; } }; } };
function C() {}
(async () => {
  log.push((await Array.fromAsync([1, Promise.resolve(2)])).join());
  log.push((await Array.fromAsync(ag(), (x, k) => Promise.resolve(x * 10 + k))).join());
  log.push((await Array.fromAsync({ length: 2, 0: "a", 1: Promise.resolve("b") })).join());
  var c = await Array.fromAsync.call(C, [1]);
  log.push(c instanceof C, c.length);
  try { await Array.fromAsync(it, () => { throw new SyntaxError; }); } catch (e) { log.push(e.name, closed); }
  try { await Array.fromAsync([1], 5); } catch (e) { log.push(e.name); }
})().then(() => { globalThis.out = log.join(" "); });
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("out");
    let g = vm.global_object();
    assert_eq!(display(vm.get(g, a).unwrap()), "1,2 10,21 a,b true 1 SyntaxError 1 TypeError");
}

/// An async arrow (or generator) in an async function (or generator)
/// captures the outer one's variables, which live in its scope object:
/// the inner one has a scope object of its own, of the same name.
#[test]
fn async_closures_in_async_functions() {
    let src = r#"
var out = [];
function* g() { var x = 5; var h = function* () { yield x; x++; }; yield* h(); yield x; }
out.push([...g()].join("/"));
(async function () {
  var d = 1;
  var f = async () => { d += 1; return d; };
  out.push(await f(), d);
})().then(() => { globalThis.result = out.join(); });
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("result");
    let g = vm.global_object();
    assert_eq!(display(vm.get(g, a).unwrap()), "5/6,2,2");
}

/// `DisposableStack`, `AsyncDisposableStack` and `SuppressedError`:
/// resources disposed of last first, errors suppressing the earlier ones.
#[test]
fn disposable_stacks() {
    let src = r#"
var log = [];
var s = new DisposableStack();
s.use({ [Symbol.dispose]() { log.push("use"); } });
s.adopt(7, v => log.push("adopt " + v));
s.defer(() => log.push("defer"));
var moved = s.move();
log.push(s.disposed, moved.disposed);
moved.dispose();
log.push(moved.disposed);
var t = new DisposableStack();
t.defer(() => { throw new TypeError("first"); });
t.defer(() => { throw new RangeError("second"); });
try { t.dispose(); } catch (e) {
  log.push(e.name, e.error.name, e.suppressed.name, Object.keys(e).length);
}
try { new DisposableStack().use(1); } catch (e) { log.push(e.name); }
try { s.use(null); } catch (e) { log.push(e.name); }
var a = new AsyncDisposableStack();
a.use({ [Symbol.dispose]() { log.push("sync in async"); } });
a.use({ async [Symbol.asyncDispose]() { log.push("async"); } });
a.disposeAsync().then(() => { log.push(a.disposed); globalThis.result = log.join(); });
var e = new SuppressedError(1, 2, "m");
log.push(e.error, e.suppressed, e.message, e instanceof Error);
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("result");
    let g = vm.global_object();
    assert_eq!(
        display(vm.get(g, a).unwrap()),
        "true,false,defer,adopt 7,use,true,SuppressedError,TypeError,RangeError,0,TypeError,ReferenceError,async,1,2,m,true,sync in async,true"
    );
}

/// `using` and `await using`: resources disposed of, last first, when
/// their block, function body or loop iteration ends however it ends;
/// errors suppressing the earlier ones; not at a script's top level nor
/// in a `case`.
#[test]
fn using_declarations() {
    let src = r#"
var log = [];
function res(n, fail) { return { [Symbol.dispose]() { log.push("dispose " + n); if (fail) throw new Error(n); } }; }
{ using a = res("a"), b = res("b"); log.push("body"); }
function f() { using x = res("x"); return "ret"; }
log.push(f());
try { { using y = res("y", true); throw new TypeError("body"); } } catch (e) {
  log.push(e.name + ":" + e.error.message + "/" + e.suppressed.name);
}
for (using z of [res("z1"), res("z2")]) log.push("iter");
for (using w = res("w"); ;) { log.push("loop"); break; }
var using = 5, of = [[1]];
for (using of of [0]) log.push("ident " + using);
for (const k of ["using q = null", "switch (1) { case 1: using q = null; }", "{ using [q] = null; }"]) {
  try { eval(k); } catch (e) { log.push(e.name); }
}
log.push(eval("4; { using c = null; }"));
(async function () {
  { await using p = { async [Symbol.asyncDispose]() { log.push("async dispose"); } }; log.push("async body"); }
  log.push("after");
})().then(() => { globalThis.result = log.join(" | "); });
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    let a = vm.intern("result");
    let g = vm.global_object();
    assert_eq!(
        display(vm.get(g, a).unwrap()),
        "body | dispose b | dispose a | dispose x | ret | dispose y | SuppressedError:y/TypeError | iter | dispose z1 | \
         iter | dispose z2 | loop | dispose w | ident 1 | SyntaxError | SyntaxError | ReferenceError | 4 | async body | \
         async dispose | after"
    );
}

/// Files `files` (name, text) in a directory of their own, for modules to
/// import each other: its path.
fn module_dir(test: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("cloison-{test}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    for (name, text) in files {
        std::fs::write(dir.join(name), text).unwrap();
    }
    dir
}

/// Module `main` of `dir` run (its jobs too): its `globalThis.result`.
fn run_module(dir: &Path, main: &str) -> Result<String, String> {
    let mut vm = Vm::new();
    let r = vm.run_module(dir.join(main).to_str().unwrap());
    vm.verify_heap();
    match r {
        Ok(()) => {
            let a = vm.intern("result");
            let g = vm.global_object();
            Ok(display(vm.get(g, a).unwrap()))
        }
        Err(cloison::vm::ModuleError::Early(m)) => Err(format!("early: {m}")),
        Err(cloison::vm::ModuleError::Thrown(e)) => Err(vm.uncaught_message(&e)),
    }
}

/// Modules: imports are live bindings to their exporters' variables
/// (uninitialized ones too, in a cycle), default exports name anonymous
/// functions and classes `default`, re-exports resolve through `export *`
/// and `export … from`, and a namespace object reads them all, sorted.
#[test]
fn modules_bind_live() {
    let dir = module_dir(
        "live",
        &[
            (
                "main.mjs",
                r#"
import { count, inc, later } from "./counter.mjs";
import f, * as all from "./reexports.mjs";
import cls from "./cls.mjs";
import value from "./value.mjs";
let out = [];
out.push(count); inc(); out.push(count);
try { count = 0; } catch (e) { out.push(e.name); }
out.push(f.name, f(), cls.name, value);
out.push(Object.keys(all).join("/"), all[Symbol.toStringTag], all.count, all.renamed, typeof all.ns.inc);
out.push(Object.getPrototypeOf(all), Object.isExtensible(all), Reflect.deleteProperty(all, "count"), Reflect.set(all, "count", 1));
out.push(JSON.stringify(Object.getOwnPropertyDescriptor(all, "count")));
out.push(later(), this, typeof arguments, import.meta === import.meta, Object.getPrototypeOf(import.meta));
globalThis.result = out.join();
"#,
            ),
            (
                "counter.mjs",
                "export let count = 1; export function inc() { count++; }\nimport { early } from './cycle.mjs';\nexport function later() { return early; }",
            ),
            (
                "cycle.mjs",
                "import { count } from './counter.mjs';\nexport let early = 'cycle';\ntry { count; } catch (e) { early = e.name; }",
            ),
            (
                "reexports.mjs",
                "export * from './counter.mjs';\nexport { count as renamed } from './counter.mjs';\nexport * as ns from './counter.mjs';\nexport default function () { return 'fn'; }",
            ),
            ("cls.mjs", "export default class {}"),
            ("value.mjs", "export default 6 * 7;"),
        ],
    );
    assert_eq!(
        run_module(&dir, "main.mjs").unwrap(),
        "1,2,TypeError,default,fn,default,42,count/default/inc/later/ns/renamed,Module,2,2,function,\
         ,false,false,false,{\"value\":2,\"writable\":true,\"enumerable\":true,\"configurable\":false},\
         ReferenceError,,undefined,true,"
    );
}

/// A module whose own code calls `eval` exports its `let`, `const` and
/// `class` bindings still (#96): its scope object, which importers read,
/// holds them; the `eval`'s code sees them there, uninitialized ones
/// throwing, a `const` not assignable. SpiderMonkey's values.
#[test]
fn modules_with_eval_export_lexicals() {
    let dir = module_dir(
        "eval",
        &[
            (
                "main.mjs",
                "import { a, b, C, d, v, setA, out } from './m.mjs';\nconst r = [a, b, typeof C, d, v];\nsetA();\nglobalThis.result = r.concat(a, d, out).join();",
            ),
            (
                "m.mjs",
                "const out = [];\nconst f = (s) => { try { return eval(s); } catch (e) { return e.name; } };\nout.push(f('typeof a'));\n\
                 try { eval('a'); } catch (e) { out.push(e.name); }\nexport let a = 1; export const b = 2; export class C {} let d = 4; export { d, out };\n\
                 export var v = 5; export function setA() { a = 10; d = 40; }\ntry { eval('b = 3'); } catch (e) { out.push(e.name); }\n\
                 out.push(f('b = 3'), eval('a + b'), f('d'), eval('let a = 9; a'), eval('a = 6'), a);",
            ),
        ],
    );
    assert_eq!(
        run_module(&dir, "main.mjs").unwrap(),
        "6,2,function,4,5,10,40,ReferenceError,ReferenceError,TypeError,TypeError,3,4,9,6,6"
    );
}

/// An arrow of a module's calls `eval`, the module's own code not (#124):
/// the module has no `new.target`, nor `arguments` (a global's by the
/// name, then), for the `eval`'s code to see, its `this` undefined; in a
/// function, the function's, past more top-level variables than the
/// `eval`'s code sees. In the interpreter and tiered up. SpiderMonkey's
/// values.
#[test]
fn modules_eval_in_arrows() {
    let vars: String = (0..100).map(|i| format!("let l{i} = {i}; var v{i} = {i};\n")).collect();
    let main = format!(
        "{vars}const r = [], t = f => {{ try {{ return String(f()); }} catch (e) {{ return e.name; }} }};\n\
         r.push(t(() => eval('l9 + l70')), t(() => eval('new.target')), t(() => (() => eval('r.push(0); new.target'))()));\n\
         r.push(t(() => eval(\"eval('new.target')\")), t(() => eval('(() => new.target)()')), t(() => eval('this')));\n\
         r.push(t(() => (() => eval('this'))()), t(() => eval('arguments')), t(() => eval('typeof arguments')));\n\
         function F() {{ return [(() => eval('new.target'))(), (() => (() => eval(\"eval('new.target')\"))())(), (() => eval('arguments.length'))()]; }}\n\
         r.push(new F(1, 2).map(x => x === F ? 'F' : x).join('/'), F().join('/'));\n\
         class C {{ x = (() => eval('String(new.target)'))(); }}\n\
         r.push(new C().x);\n\
         globalThis.arguments = 'global';\n\
         r.push(t(() => eval('arguments')), t(() => eval('new.target')));\n\
         globalThis.result = r.join();"
    );
    let dir = module_dir(
        "eval-arrows",
        &[
            ("main.mjs", &main),
            ("top.mjs", "eval('');\ntry { (() => eval('new.target'))(); } catch (e) { globalThis.result = e.name; }"),
        ],
    );
    let want = "79,SyntaxError,SyntaxError,SyntaxError,SyntaxError,undefined,undefined,ReferenceError,undefined,\
                F/F/2,//0,undefined,global,SyntaxError";
    for budget in [i64::MAX, 1, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run_module(dir.join("main.mjs").to_str().unwrap()).unwrap_or_else(|_| panic!("at {budget}"));
        vm.verify_heap();
        let (a, g) = (vm.intern("result"), vm.global_object());
        assert_eq!(display(vm.get(g, a).unwrap()), want, "tier at {budget}");
    }
    // (With an `eval` of its own too.)
    assert_eq!(run_module(&dir, "top.mjs").unwrap(), "SyntaxError");
}

/// A module's direct `eval` sees all its top-level bindings (#173), 300
/// `let`s and a `var` after them, which it assigns: in the module's own
/// code (no longer more registers than there are), in an arrow and in a
/// function. In the interpreter and tiered up. SpiderMonkey's values.
#[test]
fn modules_eval_sees_many_top_level_bindings() {
    let lets: String = (0..300).map(|i| format!("let z{i} = {i};\n")).collect();
    let main = format!(
        "{lets}var v = 5;\nconst r = [eval('z299 + v')];\neval('z299 = 1; v = 2; z0 = 7');\nr.push(z299, v, z0);\n\
         const k = () => eval('z0 + z298 + v');\nfunction inner() {{ return eval('z1 + v'); }}\n\
         r.push(k(), inner());\nglobalThis.result = r.join();"
    );
    let dir = module_dir("eval-many", &[("main.mjs", &main)]);
    for budget in [i64::MAX, 1, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run_module(dir.join("main.mjs").to_str().unwrap()).unwrap_or_else(|_| panic!("at {budget}"));
        vm.verify_heap();
        let (a, g) = (vm.intern("result"), vm.global_object());
        assert_eq!(display(vm.get(g, a).unwrap()), "304,1,2,7,307,3", "tier at {budget}");
    }
}

/// A module's direct `eval` sees its imports as the live, immutable
/// bindings they are (#151): an assignment to one (named, renamed, a
/// namespace; in the module's code, an arrow, a function) a TypeError,
/// whatever the other bindings are; a read after a call changing it the
/// new value, in an `eval` in the `eval`'s code and a closure it makes
/// too; one whose target is not initialized yet, in a cycle, a
/// ReferenceError read, a TypeError assigned (ECMA-262 9.1.1.5.1
/// GetBindingValue; 9.1.1.1.5 SetMutableBinding, step 5: the import
/// binding itself is initialized, 9.1.1.5.5). SpiderMonkey's values.
#[test]
fn modules_eval_sees_imports_live() {
    let dir = module_dir(
        "eval-imports",
        &[
            (
                "main.mjs",
                "import { live, bump } from './lib.mjs';\nimport * as ns from './lib.mjs';\nimport { live as l2 } from './lib.mjs';\n\
                 import { early } from './cycle.mjs';\n\
                 const r = [], t = f => { try { return String(f()); } catch (e) { return e.name; } };\n\
                 let x = 1;\n\
                 r.push(t(() => eval('live = 3')), t(() => eval('l2++')), t(() => eval('ns = 1')), live);\n\
                 r.push(t(() => (function () { return eval('bump(), [live, l2, ns.live].join(\"/\")'); })()));\n\
                 try { eval('x = 2; live = 3'); } catch (e) { r.push(e.name, x); }\n\
                 r.push(eval('bump(), live'), eval('eval(\"bump(), live\")'), eval('() => live')(), early);\n\
                 bump();\nr.push(live, l2, t(() => (() => eval('live'))()));\n\
                 export let late = 1;\nglobalThis.result = r.join();",
            ),
            ("lib.mjs", "export let live = 1; export function bump() { live++; }"),
            (
                "cycle.mjs",
                "import { late } from './main.mjs';\n\
                 export let early = [() => eval('late'), () => eval('late = 2')].map(f => { try { return f(); } catch (e) { return e.name; } }).join('/');",
            ),
        ],
    );
    assert_eq!(
        run_module(&dir, "main.mjs").unwrap(),
        "TypeError,TypeError,TypeError,1,2/2/2,TypeError,2,3,4,4,ReferenceError/TypeError,5,5,5"
    );
}

/// A module's direct `eval` sees any number of its imports: given in
/// parts of the registers left (52 of them past the room there was for
/// one part of 48, 300 past any one part), live and not assignable.
/// SpiderMonkey's values.
#[test]
fn modules_eval_sees_many_imports() {
    for n in [52, 300] {
        let lib: String = (0..n).map(|i| format!("export let x{i} = {i};\n")).collect();
        let names: Vec<String> = (0..n).map(|i| format!("x{i}")).collect();
        let main = format!(
            "import {{ {} }} from './lib.mjs';\nimport {{ bump }} from './bump.mjs';\n\
             let e; try {{ eval('x0 = 1'); }} catch (err) {{ e = err.name; }}\n\
             globalThis.result = [eval('x0 + x{}'), e, eval('bump(), x1')].join();",
            names.join(", "),
            n - 1
        );
        let lib = format!("{lib}export function bump2() {{ x1 = 10; }}");
        let dir = module_dir(
            &format!("eval-imports-{n}"),
            &[
                ("main.mjs", &main),
                ("lib.mjs", &lib),
                ("bump.mjs", "import { bump2 } from './lib.mjs';\nexport const bump = bump2;"),
            ],
        );
        assert_eq!(run_module(&dir, "main.mjs").unwrap(), format!("{},TypeError,10", n - 1), "{n} imports");
    }
}

/// Top-level `await`: a module waits for its async dependencies, which
/// run in order; `import()` evaluates a module once and resolves to its
/// namespace; a JSON module's default export is its value.
#[test]
fn modules_await_and_import() {
    let dir = module_dir(
        "await",
        &[
            (
                "main.mjs",
                "import { log } from './log.mjs';\nimport './a.mjs';\nimport './b.mjs';\nlog.push('main');\nconst ns = await import('./a.mjs');\nconst again = await import('./a.mjs');\nimport data from './data.json' with { type: 'json' };\nlog.push(ns === again, ns.a, data.list.length);\ntry { await import('./missing-export.mjs'); } catch (e) { log.push(e.name); }\nglobalThis.result = log.join();",
            ),
            ("log.mjs", "export const log = [];"),
            (
                "a.mjs",
                "import { log } from './log.mjs';\nlog.push('a start');\nawait null;\nlog.push('a end');\nexport const a = 'A';",
            ),
            ("b.mjs", "import { log } from './log.mjs';\nlog.push('b');"),
            ("data.json", "{ \"list\": [1, 2, 3] }"),
            ("missing-export.mjs", "import { nothing } from './log.mjs';"),
        ],
    );
    assert_eq!(run_module(&dir, "main.mjs").unwrap(), "a start,b,a end,main,true,A,3,SyntaxError");
}

/// A module importing more names than a function has registers (#139):
/// past `IMPORT_PARAMS`, its imports' holders are in an array, and they
/// stay live bindings — named, default and namespace imports, re-exports
/// of them, closures' references, an uninitialized one in a cycle — in
/// the interpreter and through the JIT, imported with `import()` too.
/// SpiderMonkey's values.
#[test]
fn modules_import_many_names() {
    for n in [160, 1000, 5000] {
        let mut big = String::new();
        let mut cyc = String::new();
        for k in 0..n {
            big.push_str(&format!("import {{ v as v{k} }} from './m.mjs';\n"));
            cyc.push_str(&format!("import {{ a as a{k} }} from './cyc_a.mjs';\n"));
        }
        let (last, mid) = (n - 1, n / 2);
        big.push_str(&format!(
            "import d, {{ bump }} from './m.mjs';\nimport * as ns from './m.mjs';\nimport {{ rv, rns }} from './re.mjs';\n\
             export const out = [v0 + v{last}, d, ns.v, rv, rns.v];\nbump();\nconst f = () => v{last} + v{mid};\n\
             out.push(v{last}, f(), ns.v, rv, rns.v);\ntry {{ v{last} = 0; }} catch (e) {{ out.push(e.name); }}\n\
             let s = 0;\nfor (let i = 0; i < 3000; i++) s += v{last} + v{mid};\nout.push(s);\nexport {{ v{last} as last }};\n\
             export function read() {{ return v0 + v{last}; }}\n"
        ));
        cyc.push_str(&format!(
            "export const early = [];\ntry {{ a{last}; }} catch (e) {{ early.push(e.name); }}\n\
             try {{ (() => a{mid})(); }} catch (e) {{ early.push(e.name); }}\nexport function touch() {{ return a0 + a{last}; }}\n"
        ));
        let dir = module_dir(
            &format!("imports{n}"),
            &[
                (
                    "main.mjs",
                    "import { bump } from './m.mjs';\nimport { seen } from './cyc_a.mjs';\nimport { early } from './cyc_b.mjs';\n\
                     const ns = await import('./big.mjs');\nconst r = [...ns.out, ns.last, ns.read()];\nbump();\n\
                     r.push(ns.last, ns.read(), seen, ...early);\nglobalThis.result = r.join();",
                ),
                ("m.mjs", "export let v = 1;\nexport function bump() { v++; }\nexport default 7;"),
                ("re.mjs", "export { v as rv } from './m.mjs';\nexport * as rns from './m.mjs';"),
                ("big.mjs", &big),
                ("cyc_a.mjs", "import { touch } from './cyc_b.mjs';\nexport let a = 2;\nexport const seen = touch();"),
                ("cyc_b.mjs", &cyc),
            ],
        );
        for budget in [i64::MAX, 1] {
            let mut vm = Vm::new();
            vm.set_tier_back_edges(budget);
            let r = vm.run_module(dir.join("main.mjs").to_str().unwrap());
            vm.verify_heap();
            assert!(r.is_ok(), "{n} imports, tier at {budget}");
            let a = vm.intern("result");
            let g = vm.global_object();
            assert_eq!(
                display(vm.get(g, a).unwrap()),
                "2,7,1,1,1,2,4,2,2,2,TypeError,12000,2,4,3,6,4,ReferenceError,ReferenceError",
                "{n} imports, tier at {budget}"
            );
        }
    }
}

/// A module's early errors: before any of the graph runs.
#[test]
fn module_early_errors() {
    let dir = module_dir(
        "early",
        &[
            ("dup.mjs", "globalThis.ran = 1; export let a; export { a };"),
            ("undeclared.mjs", "export { nope };"),
            ("unresolved.mjs", "import { nope } from './dup2.mjs';"),
            ("dup2.mjs", "export const x = 1;"),
            ("lex.mjs", "import { x } from './dup2.mjs'; function x() {}"),
            ("html.mjs", "<!-- not a comment\n"),
            ("arguments.mjs", "globalThis.result = typeof arguments; await 0;"),
        ],
    );
    for (f, what) in [
        ("dup.mjs", "twice"),
        ("undeclared.mjs", "not declared"),
        ("unresolved.mjs", "no export"),
        ("lex.mjs", "already"),
    ] {
        let e = run_module(&dir, f).unwrap_err();
        assert!(e.starts_with("early: SyntaxError") && e.contains(what), "{f}: {e}");
    }
    assert!(run_module(&dir, "html.mjs").unwrap_err().starts_with("early: SyntaxError"));
    assert_eq!(run_module(&dir, "arguments.mjs").unwrap(), "undefined");
    assert!(compile_error("import x from 'y';").contains("module"));
    assert!(compile_error("import.meta").contains("module"));
}

/// `SharedArrayBuffer`, `Atomics` and `Atomics.waitAsync` on one agent.
#[test]
fn shared_buffers_and_atomics() {
    let src = r#"
        var sab = new SharedArrayBuffer(8, { maxByteLength: 16 });
        var ta = new Int32Array(sab);
        var out = [Object.prototype.toString.call(sab), sab.growable, ta.length];
        Atomics.store(ta, 0, 5);
        out.push(Atomics.add(ta, 0, 3), Atomics.compareExchange(ta, 0, 8, 1), Atomics.exchange(ta, 0, 2), Atomics.load(ta, 0));
        sab.grow(16);
        out.push(ta.length, sab.slice(4).byteLength);
        out.push(Atomics.wait(ta, 0, 0, 0), Atomics.wait(ta, 0, 2, 1), Atomics.notify(ta, 0));
        var big = new BigInt64Array(new SharedArrayBuffer(8));
        out.push(Atomics.sub(big, 0, 5n), big[0]);
        try { Atomics.wait(new Int32Array(4), 0, 0); } catch (e) { out.push(e.name); }
        try { Atomics.add(new Float64Array(sab), 0, 1); } catch (e) { out.push(e.name); }
        var timed = Atomics.waitAsync(ta, 1, 0, 5);
        var woken = Atomics.waitAsync(ta, 2, 0);
        out.push(timed.async, Atomics.waitAsync(ta, 1, 7).value);
        timed.value.then(v => out.push("1 " + v));
        woken.value.then(v => out.push("2 " + v));
        Promise.resolve().then(() => out.push(Atomics.notify(ta, 2)));
        globalThis.out = out;
        out.length
    "#;
    for _ in 0..2 {
        let mut vm = Vm::new();
        let module = cloison::js::compile(src).unwrap();
        vm.run(&module).unwrap();
        let module = cloison::js::compile("out.join()").unwrap();
        let r = vm.run(&module).map(display).unwrap();
        assert_eq!(
            r,
            "[object SharedArrayBuffer],true,2,5,8,1,2,4,12,not-equal,timed-out,0,0,-5,TypeError,TypeError,true,not-equal,1,2 ok,1 timed-out"
        );
    }
}

/// The tests starting agents, one at a time: agents are the process's,
/// and a broadcast goes to every one (another test's, blocked in
/// `Atomics.wait`, would never take it).
static AGENT_TESTS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// `$262.agent`: agents on threads of their own, blocked in
/// `Atomics.wait` until the main one notifies them.
#[test]
fn agents_wait_and_are_notified() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let src = r#"
        var sab = new SharedArrayBuffer(8);
        var ta = new Int32Array(sab);
        for (var i = 0; i < 2; i++) {
            $262.agent.start(`
                $262.agent.receiveBroadcast(function (sab) {
                    var ta = new Int32Array(sab);
                    Atomics.add(ta, 1, 1);
                    $262.agent.report(Atomics.wait(ta, 0, 0));
                    $262.agent.leaving();
                });
            `);
        }
        $262.agent.broadcast(sab);
        var woken = 0;
        while (woken < 2) woken += Atomics.notify(ta, 0);
        var reports = [];
        while (reports.length < 2) {
            var r = $262.agent.getReport();
            if (r !== null) reports.push(r);
        }
        reports.join() + "," + woken + "," + Atomics.load(ta, 1)
    "#;
    assert_eq!(value(src), "ok,ok,2,2");
}

/// A waiter on a buffer found dead keeps its block: a new buffer (whose
/// block malloc would give the freed one's address) has no waiters
/// (issue #99: `notify` on it woke the old ones).
#[test]
fn notify_on_a_new_buffer_wakes_no_dead_buffers_waiter() {
    let src = r#"
        var out = [];
        for (var k = 0; k < 10; k++)
            Atomics.waitAsync(new Int32Array(new SharedArrayBuffer(4096)), 0, 0, 10).value.then(v => out.push(v));
        $262.gc();
        var n = 0;
        for (var i = 0; i < 10; i++) n += Atomics.notify(new Int32Array(new SharedArrayBuffer(4096)), 0);
        out.push(n);
        globalThis.out = out;
    "#;
    let mut vm = Vm::new();
    vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    let r = vm.run(&cloison::js::compile("out.join()").unwrap()).map(display).unwrap();
    assert_eq!(r, format!("0{}", ",timed-out".repeat(10)));
}

/// Another agent's grow, seen by length-tracking views once it happens
/// before (an `Atomics.load` reading what the agent stored after it, a
/// report it sent after it) (issue #103: they kept the old length).
#[test]
fn views_see_another_agents_grow() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let src = r#"
        $262.agent.start(`$262.agent.receiveBroadcast(function (sab) {
            sab.grow(32);
            Atomics.store(new Int32Array(sab), 0, 1);
            sab.grow(64);
            $262.agent.report("grown");
            $262.agent.leaving();
        });`);
        var sab = new SharedArrayBuffer(16, { maxByteLength: 64 });
        var ia = new Int32Array(sab), dv = new DataView(sab, 4);
        $262.agent.broadcast(sab);
        while (Atomics.load(ia, 0) === 0);
        var out = [ia.length >= 8, ia[7], dv.byteLength >= 28];
        while ($262.agent.getReport() === null);
        out.push(ia.length, ia[15], dv.byteLength, dv.getInt32(56));
        out.join()
    "#;
    assert_eq!(value(src), "true,0,true,16,0,60,0");
}

/// Another agent's grow, seen once it happens before through a waiter
/// list's critical section: the agent's `notify` and, later, this one's
/// `wait` that times out, `waitAsync` with no time, `notify`, an async
/// waiter that times out (they kept the old length). (The agent's plain
/// store, after its `notify`, only orders them in time.)
#[test]
fn views_see_a_grow_through_a_critical_section() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    for (i, what) in [
        "Atomics.wait(ia, 0, 0, 0)",
        "Atomics.waitAsync(ia, 0, 0, 0).value",
        "Atomics.notify(ia, 0)",
        "await Atomics.waitAsync(ia, 0, 0, 1).value",
    ]
    .iter()
    .enumerate()
    {
        let src = format!(
            r#"
            $262.agent.start(`$262.agent.receiveBroadcast(function (sab) {{
                sab.grow(64);
                new Int32Array(sab)[15] = 7;
                Atomics.notify(new Int32Array(sab), 0);
                new Int32Array(sab)[1] = 1;
                $262.agent.leaving();
            }});`);
            var sab = new SharedArrayBuffer(16, {{ maxByteLength: 64 }});
            var ia = new Int32Array(sab);
            $262.agent.broadcast(sab);
            while (ia[1] === 0) $262.agent.sleep(1);
            (async function () {{ globalThis.out = [{what}, ia.length, ia[15]].join(); }})();
        "#
        );
        let mut vm = Vm::new();
        vm.run(&cloison::js::compile(&src).unwrap()).unwrap();
        let r = vm.run(&cloison::js::compile("out").unwrap()).map(display).unwrap();
        let outcome = if i == 2 { "0" } else { "timed-out" };
        assert_eq!(r, format!("{outcome},16,7"), "{what}");
    }
}

/// Another agent's grow of one buffer, seen by the views of another it
/// grew before, once this agent reads the second's grown length through
/// `byteLength`, `slice`, `grow` (a shrink it refuses) (issue #155: only
/// the second buffer's views were brought up to date).
#[test]
fn views_see_grows_before_a_grown_length_read() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    for seen in [
        "sab2.byteLength !== 16",
        "sab2.slice(0).byteLength !== 16",
        "(() => { try { sab2.grow(16); } catch (e) { return true; } })()",
    ] {
        let src = format!(
            r#"
            $262.agent.start(`var first; $262.agent.receiveBroadcast(function (sab) {{
                if (!first) {{ first = sab; return; }}
                first.grow(64);
                sab.grow(64);
                $262.agent.leaving();
            }});`);
            var sab1 = new SharedArrayBuffer(16, {{ maxByteLength: 64 }});
            var sab2 = new SharedArrayBuffer(16, {{ maxByteLength: 64 }});
            var ia = new Int32Array(sab1), dv = new DataView(sab1, 8);
            $262.agent.broadcast(sab1);
            $262.agent.broadcast(sab2);
            while (!({seen}));
            [ia.length, dv.byteLength, sab1.byteLength, ia.length].join()
        "#
        );
        assert_eq!(value(&src), "16,56,64,16", "{seen}");
    }
}

/// A grow through one of an agent's buffers on a block (one per
/// broadcast) seen by the views of the others on it, once caught up (an
/// `Atomics.load`): the grow doesn't mark them up to date.
#[test]
fn a_grow_reaches_the_views_of_the_same_blocks_other_buffers() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let src = r#"
        $262.agent.start(`var first; $262.agent.receiveBroadcast(function (sab) {
            if (!first) { first = sab; return; }
            var ia = new Int32Array(first);
            sab.grow(64);
            Atomics.load(ia, 0);
            $262.agent.report(ia.length);
            $262.agent.leaving();
        });`);
        var sab = new SharedArrayBuffer(16, { maxByteLength: 64 });
        $262.agent.broadcast(sab);
        $262.agent.broadcast(sab);
        var r;
        while ((r = $262.agent.getReport()) === null);
        r
    "#;
    assert_eq!(value(src), "16");
}

/// Grows only, through three agents: one grows sab1; a second, once it
/// sees that (sab1's `byteLength`), grows sab2; the main one, once it
/// sees sab2 grown, has its views of sab1 up to date: the first grow
/// happens before (#155).
#[test]
fn a_grow_reaches_views_through_another_agents_grow() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let src = r#"
        $262.agent.start(`$262.agent.receiveBroadcast(function (sab) {
            sab.grow(64);
            $262.agent.leaving();
        });`);
        $262.agent.start(`var first; $262.agent.receiveBroadcast(function (sab) {
            if (!first) { first = sab; return; }
            while (first.byteLength === 16);
            sab.grow(64);
            $262.agent.leaving();
        });`);
        var sab1 = new SharedArrayBuffer(16, { maxByteLength: 64 });
        var sab2 = new SharedArrayBuffer(16, { maxByteLength: 64 });
        var ia = new Int32Array(sab1), dv = new DataView(sab1, 8);
        $262.agent.broadcast(sab1);
        $262.agent.broadcast(sab2);
        while (sab2.byteLength === 16);
        [ia.length, dv.byteLength].join()
    "#;
    assert_eq!(value(src), "16,56");
}

/// `Promise.allKeyed` and `allSettledKeyed`: an object's own enumerable
/// properties' values awaited, by the same keys.
#[test]
fn promise_keyed_combinators() {
    let src = r#"
        var out = [];
        Promise.allKeyed({ a: Promise.resolve(1), b: 2, [Symbol.iterator]: 3 }).then(r => {
            out.push(Object.getPrototypeOf(r), Reflect.ownKeys(r).length, r.a, r.b);
        });
        Promise.allSettledKeyed({ x: Promise.reject(4) }).then(r => out.push(r.x.status, r.x.reason));
        Promise.allKeyed(5).catch(e => out.push(e.name));
        globalThis.out = out;
    "#;
    let mut vm = Vm::new();
    vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    let r = vm.run(&cloison::js::compile("out.join()").unwrap()).map(display).unwrap();
    assert_eq!(r, "TypeError,,3,1,2,rejected,4");
}

/// `import defer`: the module linked, evaluated when its namespace is
/// first read (not by a symbol, or `"then"`); `import source` of a
/// JavaScript module: a SyntaxError.
#[test]
fn modules_deferred_and_source_phase() {
    let dir = module_dir(
        "defer",
        &[
            (
                "main.mjs",
                "import defer * as ns from './dep.mjs';\nconst log = globalThis.log = [];\nlog.push(ns[Symbol.toStringTag], ns.then === undefined, String(globalThis.ran));\nlog.push(ns.x, String(globalThis.ran));\nconst again = await import.defer('./dep.mjs');\nlog.push(again === ns);\ntry { await import.source('./dep.mjs'); } catch (e) { log.push(e.name); }\nglobalThis.result = log.join();",
            ),
            ("dep.mjs", "globalThis.ran = true; export const x = 1;"),
            ("source.mjs", "import source s from './dep.mjs';"),
        ],
    );
    assert_eq!(run_module(&dir, "main.mjs").unwrap(), "Deferred Module,true,undefined,1,true,true,SyntaxError");
    assert!(run_module(&dir, "source.mjs").unwrap_err().contains("SyntaxError"));
}

/// Temporal: dates, durations rounded relative to a starting point, and
/// named time zones (where the system has the database).
#[test]
fn temporal_dates_durations_and_zones() {
    let src = r#"
        var o = [];
        var d = Temporal.PlainDate.from("2024-02-29");
        o.push(String(d), d.dayOfWeek, d.add({ years: 1 }), d.until("2025-03-31", { largestUnit: "months" }));
        o.push(Temporal.Duration.from("P1DT12H").total({ unit: "hours" }));
        o.push(Temporal.Duration.from({ hours: 25 }).round({ largestUnit: "days" }));
        o.push(Temporal.Duration.from({ months: 1 }).total({ unit: "days", relativeTo: "2024-02-01" }));
        o.push(Temporal.Instant.fromEpochMilliseconds(1e12).toString({ timeZone: "+05:30" }));
        o.push(Temporal.PlainDateTime.from("2020-01-01T12:34:56.789").round({ smallestUnit: "second" }));
        o.push(Temporal.PlainYearMonth.from("2020-05").add({ months: 9 }), Temporal.PlainMonthDay.from("--02-29"));
        try { Temporal.PlainDate.from("2021-02-30"); } catch (e) { o.push(e.name); }
        o.join()
    "#;
    assert_eq!(
        value(src),
        "2024-02-29,4,2025-02-28,P13M2D,36,P1DT1H,29,2001-09-09T07:16:40+05:30,2020-01-01T12:34:57,2021-02,02-29,RangeError"
    );
    // (Named zones need the system's time zone database.)
    if std::path::Path::new("/usr/share/zoneinfo/Europe/Vienna").exists() {
        let src = r#"
            var z = Temporal.ZonedDateTime.from("2021-03-28T01:30:00+01:00[europe/vienna]");
            [z.timeZoneId, z.add({ hours: 1 }), z.hoursInDay, z.getTimeZoneTransition("next").toString()].join()
        "#;
        assert_eq!(
            value(src),
            "Europe/Vienna,2021-03-28T03:30:00+02:00[Europe/Vienna],23,2021-03-28T03:00:00+02:00[Europe/Vienna]"
        );
    }
}

/// `$262.agent.broadcast` to agents ending meanwhile (their callbacks
/// throw): one that has ended is skipped, not waited for.
#[test]
fn broadcasts_skip_agents_that_ended() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let src = r#"
        var sab = new SharedArrayBuffer(8);
        for (var k = 0; k < 5; k++) {
            for (var i = 0; i < 4; i++)
                $262.agent.start("var n = 0; $262.agent.receiveBroadcast(function () { if (++n == " + (1 + i * 3) + ") throw 1; });");
            for (var b = 0; b < 20; b++) $262.agent.broadcast(sab);
        }
        "done"
    "#;
    assert_eq!(value(src), "done");
}

/// A graph whose load failed (a module missing, in a cycle) is not kept
/// in part: importing another module of the cycle again loads the graph
/// again, and fails again, rather than linking it without the missing
/// edge. Then the missing module made, it loads.
#[test]
fn a_failed_module_graph_is_loaded_again() {
    let dir = module_dir(
        "retry",
        &[
            (
                "main.mjs",
                "var log = [];\n\
                 try { await import('./a.mjs'); log.push('a loaded'); } catch (e) { log.push(e.name); }\n\
                 try { await import('./b.mjs'); log.push('b loaded'); } catch (e) { log.push(e.name); }\n\
                 globalThis.result = log.join();",
            ),
            ("a.mjs", "import './b.mjs';\nimport './missing.mjs';\nexport const a = 1;"),
            ("b.mjs", "import './a.mjs';\nexport const b = 1;"),
        ],
    );
    assert_eq!(run_module(&dir, "main.mjs").unwrap(), "TypeError,TypeError");
}

/// Importing what is not a regular file (a device, a FIFO, a directory)
/// throws a TypeError (issue #67: `/dev/zero` was read until memory ran
/// out, a FIFO without a writer blocked forever).
#[test]
fn modules_that_are_not_regular_files_throw() {
    let dir = module_dir(
        "special",
        &[(
            "main.mjs",
            "var log = [];\n\
             for (const [s, o] of [['/dev/zero', { with: { type: 'bytes' } }], ['/dev/zero'], ['./fifo'], ['.']])\n\
               try { await import(s, o); log.push('loaded'); } catch (e) { log.push(e.name + ': ' + e.message.split(': ').pop()); }\n\
             globalThis.result = log.join();",
        )],
    );
    let fifo = std::ffi::CString::new(dir.join("fifo").to_str().unwrap()).unwrap();
    unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) };
    assert_eq!(run_module(&dir, "main.mjs").unwrap(), ["TypeError: not a regular file"; 4].join(","));
}

/// Importing a FIFO doesn't open it: a writer waiting for a reader is
/// left waiting (#67: it was opened, releasing the writer into a pipe
/// closed at once, before the check that it is no regular file).
#[test]
fn modules_that_are_fifos_are_not_opened() {
    let dir = module_dir(
        "fifo-writer",
        &[("main.mjs", "try { await import('./fifo'); } catch (e) { globalThis.result = e.message; }")],
    );
    let fifo = dir.join("fifo");
    let path = std::ffi::CString::new(fifo.to_str().unwrap()).unwrap();
    assert_eq!(unsafe { libc::mkfifo(path.as_ptr(), 0o600) }, 0);
    let (opened, wait) = std::sync::mpsc::channel();
    let writer = {
        let fifo = fifo.clone();
        std::thread::spawn(move || {
            let f = std::fs::OpenOptions::new().write(true).open(&fifo);
            opened.send(()).unwrap();
            f
        })
    };
    // (Time for the writer to reach its open.)
    std::thread::sleep(std::time::Duration::from_millis(200));
    let r = run_module(&dir, "main.mjs").unwrap();
    assert!(r.ends_with("not a regular file"), "{r}");
    let released = wait.recv_timeout(std::time::Duration::from_millis(300)).is_ok();
    // (A reader of ours lets the writer go.)
    use std::os::unix::fs::OpenOptionsExt;
    let reader = std::fs::OpenOptions::new().read(true).custom_flags(libc::O_NONBLOCK).open(&fifo).unwrap();
    writer.join().unwrap().unwrap();
    drop(reader);
    assert!(!released, "the import opened the FIFO");
}

/// An import graph deeper than the stack allows fails with a RangeError
/// that can be caught (no native stack overflow); a deep one that fits
/// loads.
#[test]
fn deep_module_graphs_load_or_throw_a_range_error() {
    let n = 20000;
    let mut files: Vec<(String, String)> = (0..n)
        .map(|i| {
            let text =
                if i + 1 < n { format!("export * from './{}.mjs';\n", i + 1) } else { "export const x = 1;\n".into() };
            (format!("{i}.mjs"), text)
        })
        .collect();
    files.push(("300.mjs".into(), "export * from './301.mjs';\n".into()));
    files.push((
        "main.mjs".into(),
        "var log = [];\n\
         try { await import('./0.mjs'); log.push('loaded'); } catch (e) { log.push(e.name); }\n\
         log.push((await import('./19700.mjs')).x);\n\
         globalThis.result = log.join();"
            .into(),
    ));
    let refs: Vec<(&str, &str)> = files.iter().map(|(a, b)| (a.as_str(), b.as_str())).collect();
    let dir = module_dir("deep", &refs);
    assert_eq!(run_module(&dir, "main.mjs").unwrap(), "RangeError,1");
}

/// Agents belong to the VM that starts them: dropped with it (an agent
/// waiting for a broadcast ends), reached by its broadcasts only, at most
/// `MAX_AGENTS` at once (cloison-fuzz BUGS.md #2: 512 dropped VMs' agents
/// outlived them and their heaps ran out).
#[test]
fn agents_end_with_their_vm() {
    let _one = AGENT_TESTS.lock().unwrap_or_else(|e| e.into_inner());
    let module = cloison::js::compile("$262.agent.start('$262.agent.receiveBroadcast(function () {})'); 1").unwrap();
    for _ in 0..600 {
        let mut vm = Vm::new();
        vm.run(&module).unwrap();
    }
    // Another VM's broadcast doesn't reach this one's agent.
    let mut a = Vm::new();
    a.run(
        &cloison::js::compile(
            "$262.agent.start('$262.agent.receiveBroadcast(function () { $262.agent.report(1); })'); 1",
        )
        .unwrap(),
    )
    .unwrap();
    let mut b = Vm::new();
    b.run(&cloison::js::compile("$262.agent.broadcast(new SharedArrayBuffer(8)); 1").unwrap()).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(50));
    let none = cloison::js::compile("$262.agent.getReport()").unwrap();
    assert_eq!(cloison::vm::display(a.run(&none).unwrap()), "null");
    let src = r#"
        var n = 0, r;
        try { for (;;) { $262.agent.start('$262.agent.receiveBroadcast(function () {})'); n++; } }
        catch (e) { r = n + " " + e.constructor.name; }
        r
    "#;
    assert_eq!(value(src), "64 TypeError");
}

/// A jump out of a `finally`'s `try` whose copy of the `finally` block
/// ends unreachable (the jump itself dead code, or the copy throwing)
/// left the `try`s further out without the range up to it: an exception
/// there escaped their `catch` (#230).
#[test]
fn a_dead_jump_out_of_a_finally_keeps_the_outer_catch() {
    let src = r#"
var fs = [
  function () { try { nope; try { if (false) return 1; } finally {} } catch (e) { return "c"; } },
  function () { try { throw 5; try { return 1; } finally {} } catch (e) { return "c" + e; } },
  function () { for (;;) { try { nope; try { if (false) break; } finally {} } catch (e) { return "c"; } } },
  function () { for (;;) { try { nope; try { if (false) continue; } finally {} } catch (e) { return "c"; } } },
  function () { L: for (;;) for (;;) { try { nope; try { if (false) break L; } finally {} } catch (e) { return "c"; } } },
  function () { try { nope; try { L: { try { if (false) break L; } finally {} } } finally {} } catch (e) { return "c"; } },
  function () { try { nope; try { try { if (false) return 1; } finally {} } finally {} } catch (e) { return "c"; } },
  function () { try { try { return 1; } finally { throw 2; } } catch (e) { return "c" + e; } },
  function () { try { nope; try { return 1; } finally { throw 2; } } catch (e) { return "c" + (e instanceof ReferenceError); } },
  function () { for (;;) { try { try { break; } finally { throw 4; } } catch (e) { return "c" + e; } } },
  function () { try { for (const x of [1]) { try { return 1; } finally { throw 3; } } } catch (e) { return "c" + e; } },
];
function* g() { try { nope; try { yield 1; if (false) return 2; } finally {} } catch (e) { yield "c"; } }
function* h() { try { try { yield 1; } finally { throw 2; } } catch (e) { yield "c" + e; } }
var r;
for (var k = 0; k < 20; k++) {
  r = fs.map((f) => { try { return f(); } catch (e) { return "escaped"; } });
  var it = h(); it.next();
  r.push(g().next().value, it.return(9).value);
}
r.join(" ");
"#;
    assert_eq!(value(src), "c c5 c c c c c c2 ctrue c4 c3 c c2");
    let first = cloison::js::compile(
        r#"
var r = [];
async function a1() { try { nope; try { await 1; if (false) return 2; } finally {} } catch (e) { return "c"; } }
async function a2() { try { try { await 0; return 1; } finally { throw 2; } } catch (e) { return "c" + e; } }
async function a3() { try { nope; for await (const x of [1]) { try { if (false) return 1; } finally {} } } catch (e) { return "c"; } }
for (const a of [a1, a2, a3]) a().then((v) => r.push(v), () => r.push("escaped"));
"#,
    )
    .unwrap();
    let second = cloison::js::compile("r.join(' ')").unwrap();
    for budget in [i64::MAX, 1, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run(&first).unwrap();
        let r = vm.run(&second).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("c c c2"), "tier at {budget}");
    }
}

#[test]
fn a_bodys_vars_are_not_its_parameters_closures() {
    // A function whose parameters have expressions has its body's `var`s
    // apart (FunctionDeclarationInstantiation step 28): a `var` of a
    // parameter's name is the body's, its value the parameter's where the
    // body starts, and the defaults' closures (and `eval`s in them) see
    // the parameter. V8 and SpiderMonkey agree (JavaScriptCore but on the
    // generator, where its closure sees the body's).
    let src = r#"
var r = [];
function t(f) { try { r.push(JSON.stringify(f())); } catch (e) { r.push(e.name); } }
t(() => (function (x, y = () => x) { var x = 2; return [x, y()]; })(1));
t(() => (function (x, y = () => x) { var x; return [x, y()]; })(1));
t(() => (function (x, y = () => { x = 5; }) { var x; y(); return x; })(1));
t(() => (function (x, y = () => { x = 5; }) { y(); return x; })(1));
t(() => (function (x = 1, y = () => x) { function x() {} return [typeof x, y()]; })());
t(() => (function (x, y = () => x) { { function x() {} } return [typeof x, y()]; })(1));
t(() => ((x, y = () => x) => { var x = 2; return [x, y()]; })(1));
t(() => ((a = () => b) => { var b = 1; return a(); })());
t(() => ({ m(x, y = () => x) { var x = 2; return [x, y()]; } }).m(1));
t(() => (function* (x, y = () => x) { var x = 2; yield [x, y()]; })(1).next().value);
t(() => (function ({ x }, y = () => x) { var x = 2; return [x, y()]; })({ x: 1 }));
t(() => (function (x, y = () => x) { for (var x of [7]); return [x, y()]; })(1));
t(() => (function (x, y = () => x) { var x = 2; return [eval("x"), y(), x]; })(1));
t(() => (function (x, y = () => eval("x")) { var x = 2; return [x, y()]; })(1));
t(() => (function (x, y = () => x) { var x = 2; eval("var x = 3"); return [x, y()]; })(1));
t(() => (function (a = eval("var z = 1"), b = () => typeof z) { return [b(), typeof z]; })());
t(() => (function (x, y = () => x) { var x = 2; return (() => eval("x"))() + y(); })(1));
t(() => (function (a, b = () => a) { var a = a + 1; return [a, b()]; })(10));
var s = 0; function hot(x, y = () => x) { var x = x * 2; return x + y(); }
for (var i = 0; i < 20000; i++) s += hot(i);
r.push(s);
r.join(" ");
"#;
    assert_eq!(
        value(src),
        r#"[2,1] [1,1] 1 5 ["function",1] ["number",1] [2,1] ReferenceError [2,1] [2,1] [2,1] [7,1] [2,1,2] [2,1] [3,1] ["number","number"] 3 [11,10] 599970000"#
    );
}

#[test]
fn a_scripts_vars_are_globals_before_it_runs() {
    // GlobalDeclarationInstantiation: every `var` (with an initializer, in
    // a `for-in`, never reached) a global property before the script's
    // first statement, not deletable (an indirect eval's are, and then
    // gone). V8, JavaScriptCore and SpiderMonkey agree.
    let src = r#"
var r = [];
var d = (n) => JSON.stringify(Object.getOwnPropertyDescriptor(globalThis, n));
r.push("a" in globalThis, d("a"), Object.prototype.hasOwnProperty.call(globalThis, "c"));
r.push(Object.keys(globalThis).includes("d"), delete globalThis.a, typeof f);
var a = 1;
if (false) { var b; }
for (var c in {}) ;
for (var d2 of []) ;
var d = d;
function f() {}
r.push(d("a"));
var s = 0;
for (var i = 0; i < 20000; i++) s = (s + i) | 0;
r.push(s);
(0, eval)("var e1 = 2;");
r.push(JSON.stringify(Object.getOwnPropertyDescriptor(globalThis, "e1")), delete globalThis.e1, "e1" in globalThis);
try { e1; r.push("read"); } catch (e) { r.push(e.name); }
r.join(" ");
"#;
    assert_eq!(
        value(src),
        r#"true {"writable":true,"enumerable":true,"configurable":false} true true false function {"value":1,"writable":true,"enumerable":true,"configurable":false} 199990000 {"value":2,"writable":true,"enumerable":true,"configurable":true} true false ReferenceError"#
    );
}

#[test]
fn dead_shapes_are_freed() {
    // Unique shapes (a property deleted), a new prototype's and frozen
    // objects' made by the hundred thousand and dropped, read through
    // inline caches that held them: full collections free them (zombies
    // kept only while something may compare with them), and the shapes
    // made in their place, at their addresses, give what V8 and
    // JavaScriptCore do.
    let src = r#"
function readAll(o) { return (o.a | 0) + (o.b | 0) + (o.k | 0); }
function make(i) { var o = { a: i, b: 1, c: 2 }; delete o.c; return o; }
var keep = [], s = 0;
for (var i = 0; i < 120000; i++) {
  var o = make(i);
  var q = Object.create({ m: i & 7, k: 3 }); q.b = 2;
  var f = Object.freeze({ a: 1, k: i & 3 });
  s += readAll(o) + readAll(q) + readAll(f) + q.m;
  if (i % 50000 == 0) keep.push(o, q);
}
for (var j = 0; j < keep.length; j++) s += readAll(keep[j]) + (keep[j].m | 0);
s
"#;
    assert_eq!(value(src), "7201530018");
    let mut vm = Vm::new();
    let r = vm.run(&cloison::js::compile(src).unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("7201530018"));
    let zombies = vm.memory_stats()[5];
    assert!(zombies < 1000, "{zombies} zombie shapes kept");
}

#[test]
fn deletes_keep_no_history_of_shapes() {
    // An object that gains and loses properties a hundred thousand times,
    // frozen objects that lost one and direct `eval`s' scopes: full
    // collections leave a few hundred shapes and their field cells (each
    // delete's shape kept the one before, for good, and every cell was
    // kept: 200K shapes, 185 MB), and the properties are what V8,
    // SpiderMonkey, JavaScriptCore and QuickJS give.
    let src = r#"
var o = {}, s = 0;
function f(p) { var l = p & 7; return eval("l * 2"); }
for (var i = 0; i < 100000; i++) {
  o["p" + (i & 31)] = i;
  delete o["p" + ((i * 7) & 31)];
  if ((i & 1023) == 0) { var q = { a: 1, b: 2, c: 3 }; delete q.b; Object.freeze(q); s += q.a + f(i); }
}
$262.gc(); $262.gc();
s + " " + Object.keys(o).join(",") + " " + Object.values(o).reduce(function (a, b) { return a + b; }, 0)
"#;
    let want = "98 p7,p10,p13,p14,p17,p20,p21,p23,p24,p26,p27,p28,p29,p30,p31 1499840";
    assert_eq!(value(src), want);
    let mut vm = Vm::new();
    let r = vm.run(&cloison::js::compile(src).unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok(want));
    let [shapes, shape_bytes, ..] = vm.memory_stats();
    assert!(shapes < 5000 && shape_bytes < 10 << 20, "{shapes} shapes kept, {shape_bytes} bytes");
}

#[test]
fn direct_evals_run_kept_code_anew() {
    // A direct `eval`'s code is kept for the same text in the same kind of
    // place (`Vm::eval_cache`), but each run is its own: its scope, `this`,
    // `new.target` and `arguments`, the closures, classes and template
    // objects it makes, its declarations and their errors, as V8,
    // SpiderMonkey, JavaScriptCore and QuickJS give.
    let src = r#"
var out = [];
function f(x) { return eval("x * 2"); }
var s = 0; for (var i = 0; i < 1000; i++) s += f(i); out.push(s);
function g() { return eval("(function () { return 1; })"); }
out.push(g() === g());
function tag(t) { return t; }
function h() { return eval("tag`a${1}b`"); }
function h2() { return eval("(function () { return tag`a`; })()"); }
out.push(h() === h(), h2() === h2());
function v(n) { eval("var w = n + 1"); return w; }
out.push(v(1), v(2));
function d() { eval("function q() { return 7; }"); return q; }
out.push(d() === d(), d()());
function sl() { eval("var z = 3"); return typeof z; }
function st() { "use strict"; eval("var z = 3"); return typeof z; }
out.push(sl(), st(), sl(), st());
function w1(o) { with (o) { return eval("a + 1"); } }
function w2(a) { return eval("a + 1"); }
out.push(w1({a: 1}), w1({a: 10}), w2(5));
function pd(a = eval("var arguments")) { }
function np() { eval("var arguments"); return 1; }
for (var k = 0; k < 2; k++) { try { pd(); out.push("no"); } catch (e) { out.push(e.constructor.name); } }
out.push(np(), np());
for (var k = 0; k < 2; k++) { try { eval("(("); } catch (e) { out.push(e.constructor.name); } }
function lc() { let y = 1; { eval("var y = 2"); } }
for (var k = 0; k < 2; k++) { try { lc(); out.push("no"); } catch (e) { out.push(e.constructor.name); } }
function th() { return eval("this"); }
var o1 = {th}, o2 = {th};
out.push(o1.th() === o1, o2.th() === o2);
function nt() { return eval("new.target"); }
out.push(new nt() !== undefined, nt() === undefined);
function ar() { return eval("arguments.length"); }
out.push(ar(1, 2), ar(1, 2, 3));
function lp(n) { return eval("var t = 0; for (var j = 0; j < n; j++) t += j; t"); }
var u = 0; for (var i = 0; i < 300; i++) u += lp(i); out.push(u);
function cl() { return eval("(class { #x = 1; static has(o) { return #x in o; } })"); }
var C1 = cl(), C2 = cl(); out.push(C1 === C2, C1.has(new C1()), C1.has(new C2()));
var x = "global";
function ind() { var x = "local"; return [(0, eval)("x"), eval("x")]; }
out.push(ind().join(), ind().join());
out.join(" ")
"#;
    assert_eq!(
        value(src),
        "999000 false false false 2 3 false 7 number undefined number undefined 2 11 6 SyntaxError SyntaxError 1 1 \
         SyntaxError SyntaxError SyntaxError SyntaxError true true true true 2 3 4455100 false true false \
         global,local global,local"
    );
}

#[test]
fn direct_evals_see_the_functions_scope_after_their_own() {
    // A direct `eval`'s scope object has no prototype: its code looks names
    // up in the function's scope object after it (it was the prototype, a
    // new one at each call: a root shape and a shape per binding each
    // call). Stores, `delete`s, nested `eval`s, `with`, shadowed bindings,
    // closures, constants and the TDZ give what V8, SpiderMonkey,
    // JavaScriptCore and QuickJS do, and calls make no shapes.
    let src = r#"
var out = [];
// var of the function assigned from eval'd code, read after.
function a1() { var v = 1; eval("v = 2"); return v; }
out.push(a1(), a1());
// var declared by an eval, assigned by a later eval, read by the function.
function a2() { eval("var w = 1"); eval("w = 5"); return w; }
out.push(a2());
// delete from eval'd code: a function's var (false), an eval's var (true).
function a3(q) { var v = 1; eval("var e = 1"); return [eval("delete v"), eval("delete e"), typeof e, eval("delete q")].join(); }
out.push(a3(1));
// block let shadowing a var, seen by eval.
function a4() { var x = "var"; { let x = "let"; return eval("x"); } }
out.push(a4());
function a5() { var x = "var"; { let x = "let"; eval("x = 'set'"); return x; } }
out.push(a5());
// nested evals: declarations go to the function, lookups see everything.
function a6(n) { let l = n; eval("eval('var nv = l + 1')"); return nv; }
out.push(a6(1), a6(2));
function a7(n) { var v = n; return eval("eval('v * 10')"); }
out.push(a7(3));
function a8() { let l = 1; eval("var t = 1; eval('t = l + 7')"); return t; }
out.push(a8());
// with around eval, and an object property shadowing the function's var.
function a9(o) { var k = "fn"; with (o) { return eval("k"); } }
out.push(a9({}), a9({k: "obj"}));
// enclosing function's variable shadowed by an eval'd var.
function b1() { var y = "outer"; function inner() { eval("var y = 'inner'"); return y; } return [inner(), y].join(); }
out.push(b1());
function b2() { var y = "outer"; function inner() { var r1 = eval("y"); eval("var y = 'inner'"); return r1 + "," + eval("y"); } return inner() + "," + y; }
out.push(b2());
// toString etc. not found as names (null prototypes).
function b3() { let z = 1; return eval("typeof toString + typeof hasOwnProperty"); }
out.push(b3());
// parameters seen and set.
function b4(pp) { eval("pp = pp + 1"); return pp; }
out.push(b4(4));
// arguments mapping through eval.
function b5(pp) { eval("arguments[0] = 9"); return pp; }
out.push(b5(1));
// closure made in eval'd code sees later changes.
function b6() { let c = 1; var f = eval("(function () { return c; })"); c = 2; return f(); }
out.push(b6());
function b7() { var c = 1; var f = eval("(function () { return c; })"); c = 3; return f(); }
out.push(b7());
// const assignment, TDZ.
function b8() { const k = 1; try { eval("k = 2"); return "no"; } catch (e) { return e.constructor.name; } }
out.push(b8());
function b9() { try { eval("tz"); return "no"; } catch (e) { return e.constructor.name; } let tz = 1; }
out.push(b9());
// strict caller: eval vars are its own.
function c1() { "use strict"; eval("var sv = 1"); return typeof sv; }
out.push(c1());
// arrow functions.
function c2() { var q = 3; return (() => eval("q + 1"))(); }
out.push(c2());
out.join(" ")
"#;
    assert_eq!(
        value(src),
        "2 2 5 false,true,undefined,false let set 2 3 30 8 fn obj inner,outer outer,inner,outer functionfunction 5 9 2 3 \
         TypeError ReferenceError undefined 4"
    );
    let mut vm = Vm::new();
    let src =
        r#"function f() { let l = 3; return eval("l * 2"); } var s = 0; for (var i = 0; i < 5000; i++) s += f(); s"#;
    let r = vm.run(&cloison::js::compile(src).unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("30000"));
    let shapes = vm.memory_stats()[0];
    assert!(shapes < 2000, "{shapes} shapes");
}

/// The scope objects made for a direct `eval` whose code has an `eval`
/// (of the bindings' accessors, one at each place among the scopes) are
/// roots until the code runs with them (issue #282): in Rust `Vec`s only,
/// a collection while the next was made, or the code compiled and loaded,
/// freed them, and the code looked its names up in freed cells. With a
/// collection at every KiB allocated, what it frees poisoned.
#[test]
fn nested_direct_evals_scope_objects_are_roots() {
    let src = r#"
function f(n, o, p) {
  let a = n;
  with (o) {
    let b = a + 1;
    with (p) {
      let c = b + 1;
      return eval("var t" + n + " = a + b + c + x + y; eval('t" + n + " + a')");
    }
  }
}
var s = 0;
for (var i = 0; i < 300; i++) s += f(i, {x: 1}, {y: 2});
s
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_gc_verify(1 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("181200"), "tier at {budget}");
        vm.verify_heap();
    }
}

/// A direct `eval` without an `eval` in its code, whose own scope object
/// no handle holds: it is a root from `Vm::eval_envs` (the review of
/// #295), with a collection at every KiB allocated, what it frees
/// poisoned. (This passes without that root too, in debug and release
/// builds: the stack word of the native that made the object still holds
/// it, which the root is there not to depend on.)
#[test]
fn a_direct_evals_scope_object_is_a_root() {
    let src = r#"
function g(n) { return eval("var v = [n, n + 1]; let w = {a: v[1]}; w.a + v.length - 2"); }
var s = 0;
for (var i = 0; i < 3000; i++) s += g(i);
s
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_gc_verify(1 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("4501500"), "tier at {budget}");
        vm.verify_heap();
    }
}

#[test]
fn mapped_arguments_objects_share_a_shape() {
    // A sloppy function's `arguments` with parameters, its elements mapped
    // to them, has a shape all such objects share, every element store on
    // the runtime path (each had a unique one, and dropped the compiled
    // code relying on the shape it left): the mapping both ways, deletes,
    // `defineProperty`, added properties, `length`, spreads and copies give
    // what V8, SpiderMonkey, JavaScriptCore and QuickJS do, and calls make
    // no shapes.
    let src = r#"
var out = [];
function m1(a, b) { arguments[0] = 10; b = 20; return [a, arguments[1], arguments.length].join(); }
out.push(m1(1, 2), m1(1));
function m2(a) { a = 5; return arguments[0]; }
out.push(m2(1));
function m3(a) { delete arguments[0]; arguments[0] = 7; return a + "," + arguments[0]; }
out.push(m3(1));
function m4(a) { Object.defineProperty(arguments, "0", { value: 3 }); var r = a; a = 9; return r + "," + arguments[0]; }
out.push(m4(1));
function m5(a) { Object.defineProperty(arguments, "0", { writable: false }); a = 4; return arguments[0] + "," + a; }
out.push(m5(1));
function m6(a) { arguments.x = 1; arguments[5] = 6; return Object.keys(arguments).join() + ";" + arguments.length; }
out.push(m6(1, 2));
function m7(a, b) { return Array.prototype.slice.call(arguments).join() + ";" + [...arguments].join() + ";" + Object.prototype.toString.call(arguments); }
out.push(m7(1, 2, 3));
function m8(a) { "use strict"; arguments[0] = 2; return a; }
out.push(m8(1));
function m9(a) { var s = 0; for (var i = 0; i < 1000; i++) { arguments[0] = i; s += a; } return s; }
var t = 0; for (var k = 0; k < 200; k++) t += m9(0); out.push(t);
function n1(a) { return arguments; }
var x1 = n1(1), x2 = n1(2); x1[0] = 5; out.push(x1[0], x2[0], Object.getPrototypeOf(x1) === Object.prototype, typeof x1.callee);
out.push(n1(4)[0]);
function n3(a, b) { arguments.length = 1; return Array.from(arguments).join() + ";" + a + b; }
out.push(n3(1, 2));
out.join(" ")
"#;
    assert_eq!(
        value(src),
        "10,20,2 10,,1 5 1,7 3,9 1,4 0,1,5,x;2 1,2,3;1,2,3;[object Arguments] 1 99900000 5 2 true function 4 1;12"
    );
    let mut vm = Vm::new();
    let src = r#"function g(x) { return x[0]; } function f(a, b) { return g(arguments) + b; }
var s = 0; for (var i = 0; i < 5000; i++) s += f(i, 1); s"#;
    let r = vm.run(&cloison::js::compile(src).unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("12502500"));
    let shapes = vm.memory_stats()[0];
    assert!(shapes < 2000, "{shapes} shapes");
}

#[test]
fn direct_evals_kept_code_is_freed() {
    // Kept direct `eval`s' code unused since a full collection goes at the
    // next: a thousand texts' (each run often, 64 kept at most) leave no
    // more code after full collections than none.
    let codes = |src: &str| {
        let mut vm = Vm::new();
        vm.run(&cloison::js::compile(src).unwrap()).unwrap();
        vm.memory_stats()[2]
    };
    let tail = "$262.gc(); $262.gc(); $262.gc();";
    let evals = format!(
        "function f(x, k) {{ return eval('(function (y) {{ return y + ' + k + '; }})(x)'); }}\n\
         var s = 0; for (var k = 0; k < 1000; k++) for (var i = 0; i < 20; i++) s += f(i, k);\n{tail}"
    );
    let none = format!(
        "function f(x, k) {{ return x + k; }}\n\
         var s = 0; for (var k = 0; k < 1000; k++) for (var i = 0; i < 20; i++) s += f(i, k);\n{tail}"
    );
    let (kept, base) = (codes(&evals), codes(&none));
    assert!(kept <= base + 8, "{kept} codes kept, {base} without the evals");
}

#[test]
fn an_objects_add_and_delete_churn_makes_no_shapes() {
    // In dictionary mode (from its second delete) an object's shape changes
    // in place: 100,000 adds and deletes leave a handful of shapes, with no
    // collection (each delete made a shape with a table and a cell for each
    // property: del.js 718 MB).
    let src = r#"
var o = {};
for (var i = 0; i < 100000; i++) { o["p" + (i & 31)] = i; delete o["p" + ((i * 7) & 31)]; }
Object.keys(o).length + " " + o.p31
"#;
    let mut vm = Vm::new();
    let r = vm.run(&cloison::js::compile(src).unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("15 99999"));
    let shapes = vm.memory_stats()[0];
    assert!(shapes < 2000, "{shapes} shapes");
}

#[test]
fn direct_evals_in_generators_read_their_boxed_bindings_bound() {
    // A generator's (or an async function's) block-scoped variables a
    // nested function uses are in boxes: a direct `eval`'s code there is
    // compiled for its site's bindings too, reading and writing a boxed one
    // through its box (`Var::BoundBox`), not through a scope object of
    // accessors for every enclosing variable made at each call (40 of them:
    // 711 ms for 20,000 calls, 35 now; a function's 25). Constants and the
    // TDZ, each iteration's binding, closures, `yield` around, an `eval` in
    // the eval'd code, `var`s, strict code and an async function give what
    // V8, SpiderMonkey, JavaScriptCore and QuickJS do. (cloison-fuzz's C10.)
    let src = r#"
var out = [];
function* g1() { let a = 1; const c = 5; let fs = [];
  for (let j = 0; j < 3; j++) { eval("a += j; fs.push(() => j * 10 + a)"); yield eval("a + c + j"); }
  try { eval("c = 1"); } catch (e) { out.push(e.constructor.name); }
  try { eval("z; let q"); } catch (e) { out.push(e.constructor.name); }
  try { (function () { eval("t"); })(); } catch (e) { out.push(e.constructor.name); }
  let t = 2;
  yield fs.map(f => f()).join();
  yield eval("eval('a * 100 + t')");
  yield eval("var vv = 7; vv + a");
  yield typeof vv;
}
out.push([...g1()].join(";"));
function* g2() { let x = 0; while (x < 5) { yield eval("x++"); } return eval("x"); }
var it = g2(), r, s = []; while (!(r = it.next()).done) s.push(r.value); out.push(s.join() + "/" + r.value);
function* g3() { "use strict"; let k = 3; yield eval("k * 2"); yield eval("var w = 1; typeof w"); yield typeof w; }
out.push([...g3()].join());
async function af() { let n = 1; await null; eval("n += 41"); let m = await eval("Promise.resolve(n)"); return eval("m + n"); }
var sum = 0; for (var i = 0; i < 3000; i++) { var gi = g2(); gi.next(); gi.next(); sum += gi.next().value; }
out.push(sum);
(function () { var v0 = 0, v1 = 1, v2 = 2;
  function* g4() { let l = 0; for (let j = 0; j < 4; j++) eval("l += v1 + v2 + j"); return l; }
  var t = 0; for (var i = 0; i < 2000; i++) t += g4().next().value; out.push(t);
})();
af().then(v => { out.push(v); globalThis.result = out.join("|"); });
"#;
    let mut vm = Vm::new();
    vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    let r = vm.run(&cloison::js::compile("result").unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(
        r.as_deref(),
        Ok(
            "TypeError|ReferenceError|ReferenceError|6;8;11;4,14,24;402;11;number|0,1,2,3,4/5|6,number,undefined|6000|36000|84"
        )
    );
}

#[test]
fn awaits_keep_their_order_without_a_promise_or_closures_of_their_own() {
    // An `await` makes no promise for its reactions' results (Await's
    // PerformPromiseThen has no capability since ES2019) and no closures:
    // its reactions name the coroutine (`promise::await_coroutine`); an
    // async function's promise has no resolving functions, a generator
    // without variables in it no scope object, a generator's state and
    // registers are one buffer and a promise's state one more. A pending
    // call kept 15 cells (784 bytes), now 4 (224; 400,000 of them: 473 MB,
    // now 216; cloison-fuzz's C09). The order of awaits, `then`s, thenables,
    // rejections caught and thrown, `for await`, async methods and
    // generators is what V8, SpiderMonkey, JavaScriptCore and QuickJS give.
    let src = r#"
var log = [];
async function a1() { log.push("a1:start"); await null; log.push("a1:1"); await Promise.resolve(); log.push("a1:2"); return "r1"; }
async function a2() { log.push("a2:start"); try { await Promise.reject(new Error("x")); } catch (e) { log.push("a2:caught " + e.message); } throw new TypeError("t"); }
async function a3() { var th = { then(r) { log.push("then called"); r(7); } }; var v = await th; log.push("a3:" + v); return v; }
async function a4() { await a1(); log.push("a4"); return await a3(); }
Promise.resolve().then(() => log.push("p1")).then(() => log.push("p2")).then(() => log.push("p3"));
a1().then(v => log.push("a1 done " + v));
a2().catch(e => log.push("a2 rejected " + e.constructor.name));
a4().then(v => log.push("a4 done " + v));
(async () => { log.push("arrow"); for await (const x of [1, Promise.resolve(2)]) log.push("fa " + x); })();
var o = { async m() { return this === o; } }; o.m().then(v => log.push("m " + v));
async function* ag() { yield 1; await null; yield 2; }
(async () => { for await (const v of ag()) log.push("ag " + v); })();
var n = 0; async function cnt() { await 0; n++; } for (var i = 0; i < 1000; i++) cnt();

Promise.resolve().then(() => Promise.resolve().then(() => Promise.resolve().then(() => Promise.resolve().then(() => Promise.resolve().then(() => Promise.resolve().then(() => Promise.resolve().then(() => globalThis.result = log.join("|") + "|n=" + n)))))));
"#;
    let mut vm = Vm::new();
    vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    let r = vm.run(&cloison::js::compile("result").unwrap()).map(display).map_err(|e| e.to_string());
    assert_eq!(
        r.as_deref(),
        Ok(
            "a1:start|a2:start|a1:start|arrow|p1|a1:1|a2:caught x|a1:1|m true|p2|a1:2|a2 rejected TypeError|a1:2|fa 1|ag 1|p3|a1 done r1|a4|then called|fa 2|a3:7|ag 2|a4 done 7|n=1000"
        )
    );
}

#[test]
fn registered_symbols_survive_collections() {
    // `Symbol.for`'s registry is the agent's, every realm's, and its
    // symbols live as long as it (GlobalSymbolRegistry): roots of every
    // collection, a young one logged when registered. It wasn't one: a
    // symbol only it held was freed, and `Symbol.for` handed the dead cell
    // out (`2 is not a symbol`). Symbols registered and dropped young and
    // old, `keyFor` after collections, registered symbols rejected as weak
    // keys, targets and tokens and others taken give what SpiderMonkey,
    // JavaScriptCore and QuickJS do (another realm's part SpiderMonkey's
    // and JavaScriptCore's with their own globals), with collections every
    // 64 KB and freed memory poisoned too.
    let src = r#"
var out = [];
function churn(n) { var junk; for (var r = 0; r < n; r++) { junk = []; for (var i = 0; i < 20000; i++) junk.push({ a: i, b: [i], c: String(i) }); } }
// Registered and dropped: only the registry holds them, young or old.
for (var i = 0; i < 2000; i++) Symbol.for("k" + i);
churn(10);
$262.gc(); $262.gc(); $262.gc();
churn(5);
var bad = 0;
for (var i = 0; i < 2000; i++) if (Symbol.keyFor(Symbol.for("k" + i)) !== "k" + i) bad++;
out.push(bad);
// The same symbol before and after collections.
var kept = Symbol.for("kept");
var desc = Symbol.for("d" + 1).description;
churn(5); $262.gc();
out.push(Symbol.for("kept") === kept, Symbol.keyFor(kept), desc, Symbol.for("d1").toString());
// Registered while the nursery fills, each one alone in the registry.
var ok = 0;
for (var i = 0; i < 300; i++) { Symbol.for("y" + i); churn(1); }
for (var i = 0; i < 300; i++) if (Symbol.keyFor(Symbol.for("y" + i)) === "y" + i) ok++;
out.push(ok);
// keyFor: unregistered, well-known, empty key, non-string keys.
out.push(Symbol.keyFor(Symbol("k1")), Symbol.keyFor(Symbol.iterator), Symbol.keyFor(Symbol.for("")) === "", Symbol.keyFor(Symbol.for(undefined)), Symbol.for(12) === Symbol.for("12"));
try { Symbol.keyFor("k1"); } catch (e) { out.push(e.constructor.name); }
// Registered symbols can't be held weakly; others can.
function tryIt(f) { try { f(); return "ok"; } catch (e) { return e.constructor.name; } }
var reg = Symbol.for("k7"), plain = Symbol("k7");
var wm = new WeakMap(), ws = new WeakSet();
out.push(tryIt(function () { wm.set(reg, 1); }), tryIt(function () { ws.add(reg); }),
  tryIt(function () { new WeakRef(reg); }), tryIt(function () { new FinalizationRegistry(function () {}).register(reg, 1); }),
  tryIt(function () { new FinalizationRegistry(function () {}).register({}, 1, reg); }),
  tryIt(function () { new FinalizationRegistry(function () {}).unregister(reg); }),
  wm.has(reg), ws.has(reg), wm.get(reg), wm.delete(reg));
out.push(tryIt(function () { wm.set(plain, 2); }), wm.get(plain), tryIt(function () { ws.add(Symbol.iterator); }), ws.has(Symbol.iterator),
  tryIt(function () { new WeakRef(plain); }));
churn(3); $262.gc();
out.push(wm.get(plain), ws.has(Symbol.iterator), tryIt(function () { wm.set(Symbol.for("k1999"), 1); }));
// Another realm's registry is this one's.
var g = $262.createRealm().global;
var s = Symbol.for("rk");
churn(2);
out.push(g.Symbol.for("rk") === s, g.Symbol.keyFor(s), g.eval("Symbol.for('rk2')") === Symbol.for("rk2"), Symbol.keyFor(g.Symbol.for("rk3")));
out.join(" ");
"#;
    let want = "0 true kept d1 Symbol(d1) 300   true undefined true TypeError TypeError TypeError TypeError TypeError \
                TypeError TypeError false false  false ok 2 ok true ok 2 true TypeError true rk true rk3";
    assert_eq!(value(src), want);
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_gc_verify(64 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok(want), "tier at {budget}");
        vm.verify_heap();
    }
}

/// A register split off for its reads in compiled code (`split_temps`)
/// stays in its slot when the interpreter reads it later: `f0.call(…)`
/// inlined calls `f0` without reading the register holding `f0`, so its
/// split ended at `.call`'s load, and an exit at `a3[p0 & 15]` (a hole
/// past the end, in the recursive call) left the slot unwritten for the
/// interpreter's `Call`, which threw "called on a non-function" (issue
/// #24). The issue's program, compiled at once or not: what V8,
/// JavaScriptCore and SpiderMonkey give.
#[test]
fn split_registers_the_interpreter_reads_after_an_exit_stay() {
    let src = r#"
var fuel = 300000;
var s1 = "aab";
var a3 = [1, , 3, , 5];
var t1 = new Int32Array(0);
function f0(d, p0, p1) {
}
function f1(d, p0) {
  if (fuel-- <= 0 || d <= 0) return p0;
  const l3 = t1[f0(3, 0, f0.call(null, 2, a3[p0 & 15], p0)) & 15];
  var l4 = 0;
  s1 = s1 + f1(d - 1, (q5 => 51)(0));
  {
    for (let k15 = 0; k15 < 13 && fuel-- > 0; k15++) {
    }
  }
}
var errors = [];
for (var i = 0; i < 200; i++) {
  try { f1(4, 0); }
  catch (e) { errors.push(i + " " + e); }
}
errors.length + " " + errors[0];
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("0 undefined"), "tier at {budget}");
    }
}

/// A register a `catch` reads keeps its slot when compiled code runs the
/// `try`: a `let` set before it and read in it is read after that by the
/// interpreter's handler (its liveness counts the handler's reads), so it
/// isn't split off into a register of its own (`jit::split_temps`), whose
/// value no exit wrote back once its reads in compiled code were done: a
/// call throwing there left the handler reading the `let`'s hole, and
/// `l3 + 1` panicked (issue #22).
/// The issue's program, compiled at once or not: what V8, JavaScriptCore
/// and SpiderMonkey give.
#[test]
fn catches_read_registers_split_off_in_their_try() {
    let src = r#"
var t0 = [];
function P0() {}
function f1() {
  try {
  } finally {
    nope;
  }
}
var out = [];
function f3() {
  let l3 = 0;
  try {
    t0[/a/.test(String(typeof l3)) ? 1 : 0] = [new P0(0).x, String(0).replace(0).length][f1(0) | 0];
  } catch (e4) {
    l3 = l3 + 1;
    out.push(l3);
  }
}
for (var i = 0; i < 10; i++) {
  f3();
}
out.join("");
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("1111111111"), "tier at {budget}");
    }
}

/// A constant load folded into the lowered code keeps its cell alive: the
/// versions compiled from that lowering read it (`Ty::of` in
/// `select_cached`), whether or not an earlier one embedded it. `f3` reads
/// the global `a2`, which `f0` sets to a new array at each call; the
/// lowering folded an array a collection had freed since, and compiling
/// `f3` again at tier 2 read its header as a shape and crashed (issue #26).
/// The issue's program, compiled at once or not: what V8, JavaScriptCore
/// and SpiderMonkey give.
#[test]
fn folded_constants_outlive_the_versions_embedding_them() {
    let src = r#"
function __mix(h, v) {
}
var __fuel = 1e9;
var __itb = { [Symbol.iterator]() { let k = 0; return { next() { return { done: k++ >= 8 }; }, }; } };
var g0 = -3;
var s1 = "abc";
var a1 = [];
var t1 = [];
var t2 = [];

o0 = Object.create(null);
function f0(d, p0) {
  a2 = [0];
  for (const v17 of __itb) {
  }
}
function f3(d, p0) {
  for (let k36 = 0; k36 < 6; k36++) {
    s1 = (s1 + { x: a1[94 & 15] / t2[k36 & 15], y: (0) }.y).slice(0);
    g0 = Math.max(f0(0), (0));
  }
  t1[Math.min(o0.z, p0 << t1[p0]) & 15] = 0;
  {
    a2[0];
    for (let k47 = 0; k47 < 4 && __fuel-- > 0; k47++) {
    }
    g0 = (0) | 0;
  }
}

for (var i = 0; i < 1000; i++) {
  var x = i * 7 + 3 | 0;
  try { f3(0); __acc = __mix(0); if (0) __samples.push(0); }
  catch (e) { __acc = __mix(0); if (0) __samples.push(0); }
  try { f3(1, i); __acc = __mix(0); if (0) __samples.push(0); }
  catch (e) { __acc = __mix(0); if (0) __samples.push(0); }
}
g0 + " " + a2.length;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("0 1"), "tier at {budget}");
    }
}

/// Compiled code that relied on a shape stays right when an object leaves
/// it: deleting the last property added (back to the shared shape it was
/// added to) makes its field mutable, dropping loads that folded its value,
/// and a unique shape's successors, whose chain of parents skips it, drop
/// the code that typed objects as having at least its slots when its
/// object leaves it. (Neither failed before: the folds and types here
/// weren't taken; the programs are the reviews' scenarios.) What V8,
/// SpiderMonkey, JavaScriptCore and QuickJS give, compiled early or not.
#[test]
fn deletes_drop_code_relying_on_the_shape_left() {
    let src = r#"
var o = { a: 1 };
o.last = { x: 7 };
function loop(n) { var s = 0; for (var i = 0; i < n; i++) { var v = o.last; s += v === undefined ? 1000 : v.x; } return s; }
var a = 0;
for (var k = 0; k < 50; k++) a += loop(10000);
delete o.last;
var folded = [a, loop(10), loop(100000)].join(" ");
function mk() { var u = { a: 1, b: 2, c: 3, d: 4 }; delete u.b; return u; }
var u = mk();
var h = { f: u };
function rd(x) { return x.f.c + x.f.d; }
var s = 0;
for (var i = 0; i < 200000; i++) s += rd(h);
u.e = 5;
for (var i = 0; i < 200000; i++) s += rd(h);
delete u.a;
var r = [];
for (var i = 0; i < 5; i++) r.push(rd(h));
u.c = 30;
r.push(rd(h));
folded + " | " + s + " " + r.join(",");
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("3500000 10000 100000000 | 2800000 7,7,7,7,7,34"), "tier at {budget}");
    }
}

/// A concurrent cycle's final pause moves cells out of fragmented blocks:
/// old objects kept one in eight among garbage, stored into each other,
/// keys of a `Map` and a `WeakMap`, while cycles run back to back, each
/// moving what it can of every other block, and checking that no
/// reference to a cell moved is left (`Vm::set_gc_evacuate_cycles`). What
/// the program reads after is what V8, SpiderMonkey, JavaScriptCore and
/// QuickJS give, compiled early or not.
#[test]
fn concurrent_cycles_evacuate_fragmented_blocks() {
    let src = r#"
var map = new Map(), wm = new WeakMap();
function mk(i) { return { i: i, s: "s" + i, a: [i, i + 1], f: function () { return i; } }; }
var old = [];
for (var r = 0; r < 60; r++) {
  var batch = [];
  for (var i = 0; i < 4000; i++) batch.push(mk(r * 4000 + i));
  for (var i = 0; i < batch.length; i += 8) old.push(batch[i]);
  for (var k = r % 97; k < old.length; k += 97) {
    var o = old[k];
    o.next = old[(k * 31 + r) % old.length];
    map.set(o, k);
    wm.set(o, o.s);
  }
  var g = "";
  for (var j = 0; j < 200; j++) g += j;
}
var sum = 0;
for (var k = 0; k < old.length; k++) {
  var o = old[k];
  sum += o.i + o.a[1] - o.f();
  if (o.s !== "s" + o.i) throw new Error("s " + k);
  if (o.next && o.next.s !== "s" + o.next.i) throw new Error("next " + k);
}
var n = 0;
map.forEach(function (v, o) { if (old[v] !== o || wm.get(o) !== o.s) throw new Error("map " + v); n++; });
sum + " " + n;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let before = cloison::gc::EVACUATED.load(std::sync::atomic::Ordering::Relaxed);
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_gc_evacuate_cycles();
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("3599910000 9446"), "tier at {budget}");
        assert!(cloison::gc::EVACUATED.load(std::sync::atomic::Ordering::Relaxed) > before, "nothing moved");
    }
}

/// Old cons strings made flat while cycles run back to back, the marker
/// sizing them meanwhile: `flatten` publishes the units' buffer only once
/// the halves are dropped, or the marker reads the right half's address
/// as the string's length and sets `END` past its block (it aborts on one
/// now). Batches kept three rounds are old by the cycle before the one
/// that flattens them.
#[test]
fn strings_flattened_while_marking_keep_their_size() {
    let src = r#"
var N = 50000, ROUNDS = 80, LAG = 3;
var parts = [];
for (var i = 0; i < 64; i++) parts.push("piece-" + i + "-abcdefghijklmnopqrstuvwxyz");
var batches = [], sum = 0;
for (var r = 0; r < ROUNDS + LAG; r++) {
  if (r < ROUNDS) {
    var a = new Array(N);
    for (var i = 0; i < N; i++) a[i] = parts[i & 63] + parts[(i * 7 + r) & 63];
    batches.push(a);
  }
  for (var j = 0, junk = []; j < 100000; j++) junk.push({ x: j });
  if (r >= LAG) {
    var old = batches[r - LAG];
    for (var i = 0; i < N; i++) sum += old[i].charCodeAt(30);
    batches[r - LAG] = null;
  }
}
sum;
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_gc_evacuate_cycles();
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("472625600"));
}

/// Old cons strings made flat while cycles run back to back, their halves
/// still reachable elsewhere: from an old array, from another cons string,
/// from a local only. The marker may read a string as the cons string it
/// was and find its halves dropped, or read them and then find it flat
/// (strings flattened in the order it traces them, so that it happens):
/// it follows no half then, and needs none, the halves being found where
/// they are kept (the final pause's verification panics on one left
/// unmarked, evacuation on a reference left to a cell moved). Half of the
/// strings are used as keys, caching their atom in the word the right half
/// was in.
#[test]
fn cons_halves_kept_elsewhere_survive_flattening_while_marking() {
    let src = r#"
var N = 10000, ROUNDS = 40, LAG = 2, box = { x: 1 };
var parts = [];
for (var i = 0; i < 64; i++) parts.push("piece-" + i + "-abcdefghijklmnopqrstuvwxyz");
function check(s, i, r) {
  // Each half is (parts[a] + parts[b]): its units must still read so.
  var a = parts[i & 63], b = parts[(i * 7 + r) & 63];
  if (s !== a + b) throw new Error("bad half " + i + " " + r);
}
function run() {
var rounds = [], sum = 0, h0 = null, h1 = null, h2 = null;
for (var r = 0; r < ROUNDS + LAG; r++) {
  if (r < ROUNDS) {
    var lefts = new Array(N), others = new Array(N), cons = new Array(N);
    for (var i = 0; i < N; i++) {
      var l = parts[i & 63] + parts[(i * 7 + r) & 63];   // a cons half
      var rt = parts[(i * 3) & 63] + parts[(i * 7 + r) & 63];
      lefts[i] = l;                                       // kept in an old array
      others[i] = rt + "!";                               // kept in another cons string
      cons[i] = l + rt;
      if (i === (N >> 1)) { h2 = h1; h1 = h0; h0 = rt; }   // a half only there and in a local
    }
    rounds.push([lefts, others, cons]);
  } else { h2 = h1; h1 = h0; h0 = null; }
  for (var j = 0, junk = []; j < 60000; j++) junk.push({ x: j });
  if (r >= LAG) {
    var k = rounds[r - LAG], lefts = k[0], others = k[1], cons = k[2], rr = r - LAG;
    for (var i = N - 1; i >= 0; i--) {
      var c = cons[i];
      // Flattened, its halves dropped (as a key, its atom cached then).
      if (i & 1) sum += c.charCodeAt(30); else sum += (box[c] | 0) + c.length;
      cons[i] = null;
      if ((i & 1023) === 0) for (var g = 0, t = []; g < 2000; g++) t.push([g]);
    }
    for (var j = 0, junk = []; j < 60000; j++) junk.push({ x: j });
    for (var i = 0; i < N; i++) {
      check(lefts[i], i, rr);
      var o = others[i];
      if (o !== parts[(i * 3) & 63] + parts[(i * 7 + rr) & 63] + "!") throw new Error("bad other " + i);
      sum += o.charCodeAt(o.length - 2) + lefts[i].charCodeAt(lefts[i].length - 1);
    }
    var h = h2, m = N >> 1, a = parts[(m * 3) & 63], b = parts[(m * 7 + rr) & 63];
    if (h !== a + b) throw new Error("bad local half " + rr);
    rounds[r - LAG] = null;
  }
}
return sum;
}
run();
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_gc_evacuate_cycles();
    vm.set_gc_verify(4 << 20);
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("149106224"));
}

/// A cons string made flat, then used as a key, caches its atom in the
/// word its right half was in. A marking that recorded that word as a
/// slot into an evacuation candidate (the half was in one) rewrites it in
/// the final pause if it holds a moved cell's address, and the marker may
/// still read it as the half: tagged, the atom never reads as a cell.
/// (Untagged, atom 4096's was the address 4 GiB into the first heap.)
#[test]
fn a_flattened_strings_cached_atom_is_no_cell() {
    let src = r#"
var box = {}, s = "abcdefghijklmnopqrstuvwxyz" + Math.random();
box[s] = 1;
box[s] + box[s];
s;
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    let s = vm.run(&module).unwrap();
    let o = cloison::object::as_object(s);
    unsafe {
        assert!(!(*o).elems.is_null(), "not flat");
        let code = cloison::value::Value::from_bits_unchecked((*o).code as u64);
        assert_ne!(code.to_bits() >> 32, 0, "no atom cached");
        assert!(!code.is_cell(), "code word {:#x} reads as a cell", code.to_bits());
    }
}

/// A direct `eval` in code an `eval` runs (its bindings through a scope
/// object of accessors, `intrinsics::eval_through_scope`) while every
/// collection moves cells: the names and the scope object it holds in Rust
/// across the eval stay put (`Vm::unmovable`), so the flush after reads
/// them, not old copies (poisoned). What V8, SpiderMonkey, JavaScriptCore
/// and QuickJS give.
#[test]
fn evals_through_scope_objects_hold_still_while_cells_move() {
    let src = r#"
function f() {
  let a = 1, b = "two";
  var c = [3];
  eval("var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i: i, s: 'x' + i }); eval('a += 10'); b += '!'; c.push(4); junk = null;");
  return a + " " + b + " " + c.join(",");
}
var r = [];
for (var k = 0; k < 3; k++) r.push(f());
r.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_gc_evacuate_all();
    vm.set_gc_verify(256 << 10);
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("11 two! 3,4 | 11 two! 3,4 | 11 two! 3,4"));
}

/// A prototype load a lowering holds (`Prop::Proto`, and a polymorphic
/// one's `CaseLoc::Proto`) while only its loop entry is compiled, then the
/// call entry compiled after collections that move cells (issue #60): the
/// holder's slot wasn't pinned on the lowering's account, so the
/// prototype moved and the later version read its old copy (poisoned).
/// What V8, SpiderMonkey, JavaScriptCore and QuickJS give.
#[test]
fn prototype_loads_a_lowering_holds_keep_their_holder_still() {
    let src = r#"
var A = { m: 7 }, B = { m: 8 };
var o = {}, p = {}, q = { x: 1 };
Object.setPrototypeOf(o, A);
Object.setPrototypeOf(p, B);
Object.setPrototypeOf(q, B);
function f(n) {
  var s = 0;
  if (n < 0) s = this.m;
  for (var i = 0; i < n; i++) s++;
  return s;
}
function g(n) {
  var s = 0;
  if (n < 0) s = this.m;
  for (var i = 0; i < n; i++) s++;
  return s;
}
f.call(o, -1);
f.call(o, 100000);
g.call(p, -1);
g.call(q, -1);
g.call(p, 100000);
$262.gc(); $262.gc();
var a = new Array(5000).fill(-1);
[a.map(f, o)[4999], a.map(g, p)[4999], a.map(g, q)[4999]].join(" ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_gc_evacuate_all();
    vm.set_gc_verify(1 << 10);
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("7 8 8"));
}

/// A regular expression's compiled pattern goes with the last `RegExp`
/// using it (issue #34): `Vm::regexps` kept the pattern of every new
/// source for good, 10 KiB each. Regexps of 20,000 sources made, run and
/// dropped leave a handful; the ones still live match after the
/// collections, as a literal in a function run again does, and a
/// pattern `compile` gave. What V8, SpiderMonkey and JavaScriptCore give.
#[test]
fn regexps_of_new_sources_are_freed_with_their_objects() {
    let src = r#"
var keep = [/a(b+)c/g, new RegExp("x(y)z", "i")];
function lit(s) { return /q(\d+)/.exec(s)[1]; }
var sum = 0;
for (var i = 0; i < 20000; i++) {
  var re = new RegExp("w" + i + "(\\d*)", i & 1 ? "g" : "");
  sum += re.exec("w" + i + "42")[1].length;
  if (i % 1000 == 0) { $262.gc(); sum += lit("q" + i).length; }
}
var c = /m(n)/;
c.compile("o(p+)", "g");
$262.gc();
keep[0].lastIndex = 1;
var m = keep[0].exec("xabbbc");
[sum, m[1], keep[0].lastIndex, keep[1].test("XYZ"), lit("q77"), c.exec("ooppp")[1], c.global].join(" ");
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("40087 bbb 6 true 77 ppp true"), "tier at {budget}");
        assert!(vm.regexp_count() < 100, "{} regexps kept", vm.regexp_count());
    }
}

/// A pattern a builtin holds across user code stays while it does, though
/// no `RegExp` uses it any more (Codex on #36): `exec` takes its object's
/// before converting the input, `replace` across its callbacks and `exec`
/// reading `lastIndex`, and the user code there `compile`s the object
/// another pattern and collects; that collection dropped the old one
/// (`Vm::prune_regexps`), the regexps made after took its memory, and the
/// builtin matched with a freed pattern (a segfault). The new patterns
/// match as the old ones, so the order the spec reads the matcher in
/// doesn't change the results: what V8, SpiderMonkey, JavaScriptCore and
/// QuickJS give.
#[test]
fn regexp_patterns_held_by_builtins_survive_a_compile_and_collection() {
    let src = r#"
var out = [];
function churn() { for (var i = 0; i < 3000; i++) new RegExp("z" + i + "(q+)\\d{" + (i % 7) + "}").exec("zq"); }
var r = /a(b)c/;
out.push(String(r.exec({ toString() { r.compile("a(b)(?:c)"); $262.gc(); churn(); $262.gc(); return "abc"; } })));
var g = /(\d)/g, n = 0;
out.push("1a2b3".replace(g, function (m) { g.compile("(\\d)" + "(?:)".repeat(++n), "g"); $262.gc(); churn(); $262.gc(); return "<" + m + ">"; }));
var s = /k(\w)/g;
s.lastIndex = { valueOf() { s.compile("k(\\w)(?:)", "g"); $262.gc(); churn(); $262.gc(); return 0; } };
out.push(String(s.exec("xkyk")));
out.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("abc,b | <1>a<2>b<3> | ky,y"), "tier at {budget}");
    }
}

/// An arrow function is a whole AssignmentExpression, not a
/// MemberExpression: nothing continues it without parentheses, neither a
/// call, a member, a tagged template, an update nor an operator (`() =>
/// {}()` was called, issue #28). After a line break, a semicolon is
/// inserted instead. What V8, JavaScriptCore and SpiderMonkey do.
#[test]
fn arrow_functions_are_not_continued() {
    for src in [
        "var r = () => { return 1; }();",
        "var r = async () => {}();",
        "var r = () => {}.x;",
        "var r = () => {}[0];",
        "var r = () => {}?.x;",
        "var r = x => {}`t`;",
        "var r = () => {} + 1;",
        "var r = () => {} || 1;",
        "var r = () => {} ? 1 : 2;",
        "var r = () => {}\n.x;",
    ] {
        assert!(cloison::js::compile(src).is_err(), "{src}");
    }
    let src = r#"
var x = 0, out = [];
var f = () => {}
(1)
var g = async y => {}
++x
out.push(typeof f, typeof g, x);
out.push((() => 1)(), (() => { return 2; })(), (a => b => a + b)(1)(2));
out.push((true ? () => 3 : () => 4)(), (y => y ? 5 : 6)(0), [() => 1, y => {}].length);
out.join(" ");"#;
    assert_eq!(value(src), "function function 1 1 2 3 3 6 2");
}

/// An array literal's `PushLiteral` reads its array's register and `acc`
/// only: lowered as a `push` call (`Insn::native` 1) whose window was the
/// array's register and the one after, it counted that next register as
/// read, so a variable written only later (`r13`, `__shp`'s) seemed live
/// and read at an entry at `f0`'s first loop, and `entry_types` typed the
/// stale word in its slot as an object and crashed in `Ty::join_shapes`
/// (issue #27). The issue's program, compiled at once or not: what V8,
/// JavaScriptCore and SpiderMonkey give.
#[test]
fn array_literal_pushes_read_no_argument_register() {
    let src = r#"
function __mix() {}
var __fuel = 300000;
var g1 = 8;
var g2 = 0x7fffffff;
var s0 = "a1b2c3";
var s2 = "0x1f";
var a0 = [1, , 3, , 5];
var a1 = new Array(0);
var a2 = Array.from(0);
o2 = { x: 42, };
var __shp = [{ x: 1 }, { y: 2, x: 3 }, { x: 4, y: 5, z: 6 }];
function f0(d, p0, p1, p2) {
  if (__fuel-- <= 0 || d <= 0) return a1.length;
  for (let k1 = 0; k1 < 2 && __fuel-- > 0; k1++) {
    g2 = g2 + a2.map(v => v | 0).sort((u, v) => u - v)[f0(d - 1) & 7] | 0;
    o2["p" + (0)] = 0;
  }
  for (let k4 = 0; k4 < 1 && __fuel-- > 0; k4++) {
    a1[s2.substring(a0[k4 & 15] / g1 & 7, [p1, g1][__shp[0].w & 1] & 15).length & 15] = s0.lastIndexOf(0);
  }
  if (0) throw { x: 1 };
  return (() => { const o = { _v: 1, get v() { return this._v; } }; return o.v; })();
}

for (var i = 0; i < 1000; i++) {
  f0(4, 0);
}
g2 + " " + a1.length;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("0 1"), "tier at {budget}");
    }
}

/// `exec`, `test` and `match` match with the pattern the regexp has once
/// the input is converted and `lastIndex` read (issue #37: RegExpBuiltinExec
/// reads the flags and the matcher after `lastIndex`, which may `compile` it
/// another, as the input's `toString` may): its flags decide where to
/// look, and its groups the array's. What V8, SpiderMonkey and
/// JavaScriptCore give.
#[test]
fn regexp_exec_matches_with_the_pattern_compiled_by_its_conversions() {
    let src = r#"
var out = [];
var r = /a(b)c/;
out.push(String(r.exec({ toString() { r.compile("x"); return "abc"; } })));
var s = /k(\w)/g;
s.lastIndex = { valueOf() { s.compile("zz", "g"); return 0; } };
out.push(String(s.exec("xkyk")));
var t = /q/;
out.push(t.test({ toString() { t.compile("w"); return "q"; } }));
var u = /(a)/;
var m = u.exec({ toString() { u.compile("(a)(b)(?<n>c)"); return "abc"; } });
out.push(m.length + " " + m[3] + " " + m.groups.n);
var v = /o/g;
v.lastIndex = { valueOf() { v.compile("o"); return 5; } };
var mv = v.exec("foo");
out.push(mv.index + " " + v.lastIndex);
var w = /e(f)/;
w.lastIndex = { valueOf() { w.compile("e(f)(g)"); return 0; } };
out.push(String("efg".match(w)));
var f = /b/y;
f.lastIndex = { valueOf() { f.compile("a", "y"); return 0; } };
out.push(f.test("ab"));
// (ToString: a symbol throws, as `String(symbol)` doesn't.)
for (var g of [function () { return /Symbol/.test(Symbol("x")); }, function () { return /Symbol/.exec(Symbol("x")); }])
  try { out.push(String(g())); } catch (e) { out.push(e.constructor.name); }
out.push(/a/.test(), String(/undefined/.exec()), /1/.test(1), String(/null/.exec(null)));
out.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(
            r.as_deref(),
            Ok(
                "null | null | false | 4 c c | 1 0 | efg,f,g | true | TypeError | TypeError | false | undefined | true | null"
            ),
            "tier at {budget}"
        );
    }
}

/// Code made at run time goes with its last closure, though the JIT took
/// it (issue #35): a code the JIT compiled, inlined or bound a call to was
/// kept for good, with its compiled code, the JIT's tables of it and what
/// those kept. Functions of fresh sources (`new Function`), compiled hot
/// and dropped, leave a bounded number of codes; a long-lived function
/// calling functions made at run time, inlining them, each replaced by
/// another between collections, still runs them right. The sums are what
/// V8 and SpiderMonkey give.
#[test]
fn code_made_at_run_time_is_freed_though_compiled() {
    let src = r#"
var out = [];
var refs = [], sum = 0;
function round(r) {
  for (var k = 0; k < 40; k++) {
    var big = [r, k, 1, 2];
    var f = new Function("big", "return function (n) { var s = 0; for (var i = 0; i < n; i++) s = (s + big[i & 3] + " + k + ") | 0; return s; };")(big);
    sum = (sum + f(3000)) | 0;
    if (k % 10 == 0) refs.push(new WeakRef(big));
  }
}
var g = null, gsum = 0;
function caller(n) { var s = 0; for (var i = 0; i < n; i++) s = (s + g(i)) | 0; return s; }
async function main() {
  for (var r = 0; r < 12; r++) {
    round(r);
    g = new Function("x", "return (x * " + (r + 2) + ") & 1023;");
    gsum = (gsum + caller(5000)) | 0;
    await null;
    $262.gc();
    gsum = (gsum + caller(100)) | 0;
  }
  $262.gc();
  out.push(sum, gsum, refs.filter(function (w) { return w.deref() === undefined; }).length >= 40);
}
main();
"#;
    let module = cloison::js::compile(src).unwrap();
    let report = cloison::js::compile("out.join(' ')").unwrap();
    // (The natives' codes, for good.)
    let natives = Vm::new().code_count();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run(&module).unwrap();
        let r = vm.run(&report).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("38160000 30835860 true"), "tier at {budget}");
        vm.verify_heap();
        let kept = vm.code_count() - natives;
        assert!(kept < 100, "{kept} codes kept (tier at {budget})");
    }
}

/// A tagged template's strings array goes with the code whose site it is
/// (#35): `Vm::template_cache` kept every one for good, a root of every
/// collection, so code made at run time with a template grew the heap at
/// each copy. Templates of functions made at run time and dropped are
/// freed; each site's array stays the same frozen one while its code
/// lives, and a live code's is kept.
#[test]
fn tagged_templates_go_with_their_code() {
    let src = r#"
var out = [];
function tag(s) { return s; }
var refs = [], same = true;
async function main() {
  for (var r = 0; r < 8; r++) {
    for (var k = 0; k < 30; k++) {
      var f = new Function("tag", "return tag`a${1}b" + (r * 100 + k) + "`;");
      var a = f(tag);
      if (a !== f(tag) || !Object.isFrozen(a) || a.raw[1] !== "b" + (r * 100 + k)) same = false;
      refs.push(new WeakRef(a));
    }
    await null;
    $262.gc();
  }
  var keep = new Function("tag", "return tag`kept`;");
  var k1 = keep(tag);
  await null;
  $262.gc(); $262.gc();
  await null;
  $262.gc();
  out.push(same, keep(tag) === k1, refs.filter(function (w) { return w.deref() === undefined; }).length >= 200);
}
main();
"#;
    let module = cloison::js::compile(src).unwrap();
    let report = cloison::js::compile("out.join(' ')").unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.run(&module).unwrap();
        let r = vm.run(&report).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("true true true"), "tier at {budget}");
        vm.verify_heap();
    }
}

/// Versions calling each other, bound to each other's (`CallSite::target`),
/// hold each other: freeing their functions dropped their compiled state
/// but not those versions, whose snapshots still named the functions'
/// codes, freed; the next marking read them (#35, a segfault with every
/// collection full, of a VM after another's in the same thread). Each
/// VM's functions compile calling each other, a collection after.
#[test]
fn versions_calling_each_other_outlive_no_freed_code() {
    let src = r#"
var total = 0;
function round(r) {
  var pair = new Function("k", "function even(n) { return n === 0 ? k : odd(n - 1) + 1; }" +
    "function odd(n) { return n === 0 ? 2 * k : even(n - 1) + 2; }" +
    "return [even, odd];")(r);
  for (var i = 0; i < 2000; i++) total = (total + pair[0](i & 31) + pair[1](i & 15)) | 0;
}
for (var r = 0; r < 20; r++) {
  round(r);
  $262.gc();
  for (var j = 0; j < 300; j++) new Function("x", "return x + " + j + ";")(j);
}
$262.gc();
total;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [2, 1000, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("2516160"), "tier at {budget}");
    }
}

/// A function the JIT took and freed leaves no table naming it (Codex on
/// #39): compiled code reading a field records its code as relying on the
/// field's type (`Vm::field_deps`, `relied_cells`), which the field's
/// generalizing reads and changes, and the JIT's list of its functions
/// (`Vm::jitted`); a freed code left there was written to then. Functions
/// of fresh sources read a long-lived object's field compiled, are freed,
/// and then the field changes type: what V8 gives.
#[test]
fn freed_codes_leave_no_field_dependencies() {
    let src = r#"
var obj = { x: 1, y: 2 }, total = 0;
function round(r) {
  for (var k = 0; k < 30; k++) {
    var f = new Function("o", "var s = 0; for (var i = 0; i < 3000; i++) s = (s + o.x + " + k + ") | 0; return s;");
    total = (total + f(obj)) | 0;
  }
}
for (var r = 0; r < 6; r++) {
  round(r);
  $262.gc();
  $262.gc();
  for (var j = 0; j < 3000; j++) new Function("a", "return a + " + j + ";")(j);
  $262.gc();
}
obj.x = 1.5;
obj.x = "s";
obj.y = {};
total + " " + obj.x;
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("8370000 s"), "tier at {budget}");
        assert_eq!(vm.codes_named_but_not_live(), 0, "tier at {budget}");
    }
}

/// Run `src` below a zeroed stretch of the native stack, so that no stale
/// word of an earlier script's frames (a cell, or an address in compiled
/// code, which the collector reads as a frame running it) is in what the
/// collections of `src` scan. The stretch is 512 KB of a test thread's
/// 2 MB: under a smaller `RUST_MIN_STACK` (below 1 MB or so) the tests
/// using this overflow their stack.
#[inline(never)]
fn below(vm: &mut Vm, src: &str) -> String {
    let pad = std::hint::black_box([0usize; 1 << 16]);
    let r = vm.run(&cloison::js::compile(src).unwrap()).map(display).unwrap();
    std::hint::black_box(&pad);
    r
}

/// Code aging (`jit::age_functions`): a function's compiled code not used
/// for a few full collections is dropped, and compiled again if it is
/// called later; one called directly from compiled code all along stays.
/// The collections run in a second script, below a zeroed stretch of the
/// stack: a stale word of the first one's frames pointing into `idle`'s
/// code would count as a frame running it. The expected values are Node's.
#[test]
fn compiled_code_idle_for_some_collections_is_dropped() {
    let warm = r#"
function idle(n) { var s = 0; for (var i = 0; i < n; i++) s += i & 3; return s; }
function busy(n) { return n < 2 ? n : busy(n - 1) + busy(n - 2); }
function caller(n) { return busy(n) + 1; }
var total = 0;
for (var i = 0; i < 3000; i++) total += idle(20) + caller(7);
"#;
    let rounds = |rounds: u32, after: &str| {
        format!(
            r#"
for (var r = 0; r < {rounds}; r++) {{
  for (var i = 0; i < 3000; i++) total += caller(7);
  $262.gc();
}}
{after}
total;
"#
        )
    };
    let run = |n: u32, after: &str| {
        let mut vm = Vm::new();
        vm.run(&cloison::js::compile(warm).unwrap()).unwrap();
        assert!(vm.jit_stencils("idle").is_some());
        let r = below(&mut vm, &rounds(n, after));
        // Retired, `idle` keeps none of its lowering; compiled, it has one.
        let (insns, tables) = vm.jit_lowering("idle").unwrap();
        let compiled = vm.jit_stencils("idle").is_some();
        assert_eq!((insns > 0, compiled || tables == 0), (compiled, true), "{insns} instructions, {tables} bytes");
        assert!(vm.jit_lowering("busy").unwrap().0 > 0);
        (r, compiled, vm.jit_stencils("busy").is_some())
    };
    assert_eq!(run(2, ""), ("216000".to_string(), true, true));
    assert_eq!(run(6, ""), ("384000".to_string(), false, true), "`idle`'s code is dropped");
    let again = "for (var i = 0; i < 3000; i++) total += idle(20);";
    assert_eq!(run(6, again), ("474000".to_string(), true, true), "and compiled again");
}

/// A function retired at a collection while a frame runs one of its
/// versions (`gc::weak_kept`, `jit::retire_functions`; the review of
/// #281). `loop`, too deep to be inlined in `warm`, inlines the closure
/// it calls, `b`, and `warm`'s code calls its version directly. From
/// inside that call (`fire`, reached by a call with too many callees to
/// be bound, so that no frame leaves its code for it), `loop` is lowered
/// again for a second closure, made and dropped at a depth the
/// collections' frames don't reach, and two collections run: the first
/// keeps what code took since the last one, the second finds the closure
/// dead, so `loop` is retired (and `other`, and the closure's code). The
/// version under way stays, kept apart, with what it refers to; its call
/// site is unbound. Then it leaves by an exit (`x.v` is a string now),
/// which `direct_exit` finishes from the version kept. The cache of
/// `loop`'s call lost its latest way, the dead closure's: `loop` runs
/// from it in the interpreter and is compiled from it again, and all of
/// it once more. The first script warms up; the second runs below a
/// zeroed stretch of the stack (see `below`). The expected values are
/// Node's.
#[test]
fn code_retired_under_a_running_frame_runs_to_its_exit() {
    let warm = r#"
var gcs = 0, ints = { v: 1 }, mode = 0, next = [1, 2, 3, 4, 0, 5];
function fire() {
  mode = 0;
  deep(100, function () { return other(mk(10 + gcs)); });
  $262.gc();
  $262.gc();
  gcs++;
  ints.v = "s";
}
var acts = [function () {}, function () {}, function () {}, function () {}, function () {}, fire];
function hook() { acts[mode](); mode = next[mode]; }
function mk(n) {
  return (0, eval)("(function () { var o = [" + n + "]; " +
    "return function (x) { hook(); return o[0] + x.v; }; })()");
}
function loop(g, x, n) { var t = 0; for (var i = 0; i < n; i++) t += g(x); return t; }
function inner(g, x, n) { return loop(g, x, n) + 1; }
function outer(g, x, n) { return inner(g, x, n) + 1; }
function warm(g, m) { var t = 0; for (var i = 0; i < m; i++) t += outer(g, ints, 5); return t; }
function other(g) { var t = 0; for (var i = 0; i < 3000; i++) t += loop(g, ints, 5); return t; }
function deep(n, f) { return n ? deep(n - 1, f) : f(); }
var out = [], b = mk(2), total = 0;
for (var k = 0; k < 20; k++) total += warm(b, 300);
out.push(total);
"#;
    let rest = r#"
mode = 5;
out.push(warm(b, 2));
ints.v = 1;
out.push(warm(b, 3000));
mode = 5;
out.push(warm(b, 2));
ints.v = 1;
out.push(warm(b, 3000), gcs);
out.join(" ");
"#;
    let mut vm = Vm::new();
    vm.run(&cloison::js::compile(warm).unwrap()).unwrap();
    assert!(vm.jit_stencils("loop").is_some() && vm.jit_stencils("warm").is_some());
    assert_eq!((vm.jit_stats("loop").unwrap().retirements, vm.jit_retired_exits()), (0, 0));
    assert_eq!(below(&mut vm, rest), "102000 002s2s2s2s2s1102s2s2s2s2s11 51000 002s2s2s2s2s1102s2s2s2s2s11 51000 2");
    assert_eq!(vm.jit_stats("loop").unwrap().retirements, 2, "`loop` is retired at each round");
    assert!(vm.jit_retired_exits() >= 1, "an exit left the version retired under its frame");
    assert!(vm.jit_stencils("loop").is_some(), "and `loop` is compiled again");
}

/// An inline cache's way whose holder died is cleared, the ways left
/// moved down (`gc::weak_kept`; the review of #281): `get` and `call`
/// read through two prototypes, the latest made and dropped at a depth
/// the collections' frames don't reach. Once it is dead (the first
/// collection keeps what the caches took since the last one, and a
/// prototype goes with its objects' shape), the caches answer for the
/// prototype left and for a new one, in the interpreter and in code
/// compiled from them. The expected values are Node's.
#[test]
fn a_dead_holders_cache_way_is_cleared() {
    let warm = r#"
var wr, out = [];
function get(o) { return o.a; }
function call(o) { return o.f(); }
function run(o, n) { var t = 0; for (var i = 0; i < n; i++) t += get(o) + call(o); return t; }
function deep(n, f) { return n ? deep(n - 1, f) : f(); }
function proto(k) { return { a: k, f: function () { return 10 * k; } }; }
var keep = Object.create(proto(1));
out.push(run(keep, 20));
deep(50, function () {
  var p = proto(2);
  wr = new WeakRef(p);
  out.push(run(Object.create(p), 20));
});
"#;
    let rest = r#"
$262.gc();
$262.gc();
$262.gc();
out.push(wr.deref() === undefined);
out.push(get(keep), call(keep), run(keep, 20), run(Object.create(proto(3)), 20));
out.push(run(keep, 5000), run(Object.create(proto(4)), 5000), get(keep), call(keep));
out.join(" ");
"#;
    for jit in [false, true] {
        // (Compiled: `run`'s loop in the first script too, the caches'
        // holders then cells its code refers to, and it is retired.)
        let warm = if jit { warm.replace("20)", "5000)") } else { warm.to_string() };
        let mut vm = Vm::new();
        vm.run(&cloison::js::compile(&warm).unwrap()).unwrap();
        let first = if jit { "55000 110000" } else { "220 440" };
        assert_eq!(below(&mut vm, rest), format!("{first} true 1 10 220 660 55000 220000 1 10"), "compiled: {jit}");
        assert!(vm.jit_stencils("run").is_some());
        assert_eq!(vm.jit_stats("run").unwrap().retirements > 0, jit, "compiled: {jit}");
    }
}

/// A VM dropped leaves none of its compiled versions (the review of #39):
/// versions calling each other, bound to each other's, and retired ones
/// whose own call sites were bound to them, held each other after their
/// functions were freed or their VM dropped, in the JIT's registry of
/// them (`jit::alive`), which then named codes' addresses other VMs' got.
/// Functions calling each other, compiled, some made at run time, then
/// the VM dropped.
#[test]
fn a_dropped_vm_leaves_no_compiled_versions() {
    let src = r#"
var total = 0;
function even(n) { return n === 0 ? 1 : odd(n - 1) + 1; }
function odd(n) { return n === 0 ? 2 : even(n - 1) + 2; }
for (var i = 0; i < 3000; i++) total = (total + even(i & 31) + odd(i & 15)) | 0;
for (var r = 0; r < 5; r++) {
  var pair = new Function("k", "function a(n) { return n === 0 ? k : b(n - 1) + 1; }" +
    "function b(n) { return n === 0 ? 2 * k : a(n - 1) + 2; }" +
    "return [a, b];")(r);
  for (var i = 0; i < 2000; i++) total = (total + pair[0](i & 31) + pair[1](i & 15)) | 0;
  $262.gc();
}
total;
"#;
    let module = cloison::js::compile(src).unwrap();
    assert_eq!(cloison::jit::alive(), (0, 0));
    for budget in [2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("516348"), "tier at {budget}");
        assert!(cloison::jit::alive().1 > 0, "tier at {budget}: compiled");
        drop(vm);
        assert_eq!(cloison::jit::alive(), (0, 0), "tier at {budget}: versions left");
    }
}

/// Property names and symbols go with the last thing naming them (issue
/// #33): an atom was kept for good, a symbol used as a key too, and each
/// key's string `for-in` made (`Vm::strings`). Objects of names and symbols
/// never seen before, made, enumerated and dropped, leave a bounded number
/// of atoms; what stays live keeps its names: a long-lived object's keys
/// and symbols (one reachable through the object alone), a string keeping
/// its atom (`Object::string_atom`) used as a key after the objects having
/// it are gone, a property added through it, `for-in` over them. With
/// freed atoms never reused, and read panicking (`set_atoms_poison`), as
/// without. What V8, SpiderMonkey and JavaScriptCore give.
#[test]
fn property_names_and_symbols_are_freed_with_their_objects() {
    let src = r#"
var keep = { a: 1 };
keep[Symbol("kept")] = 2;
var kept = Symbol("named"), key = "cached_" + 7;
keep[kept] = 3;
var probe = {};
probe[key] = 4;
probe = null;
var sum = 0;
for (var r = 0; r < 30; r++) {
  for (var k = 0; k < 300; k++) {
    var o = {};
    o["n" + r + "_" + k] = k;
    o[Symbol("s" + k)] = k;
    for (var p in o) sum += p.length;
  }
  $262.gc();
}
var again = {};
again[key] = 5;
var names = [];
for (var p in keep) names.push(p);
var syms = Object.getOwnPropertySymbols(keep).map(function (s) { return s.toString() + "=" + keep[s]; });
[sum, names.join(","), syms.join(","), again[key], again.cached_7, keep[kept]].join(" ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let base = Vm::new().atom_count();
    for (budget, poison) in [(i64::MAX, false), (2, false), (i64::MAX, true), (2, true)] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_atoms_poison(poison);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(
            r.as_deref(),
            Ok("56700 a Symbol(kept)=2,Symbol(named)=3 5 5 3"),
            "tier at {budget}, poison {poison}"
        );
        let atoms = vm.atom_count() - base;
        assert!(atoms < 2000, "{atoms} atoms kept (tier at {budget}, poison {poison})");
    }
}

/// A symbol only a live object's key names keeps its weak collections'
/// entries (Codex on #40): a full marking found such a symbol after the
/// weak collections' dead entries went (`gc::ephemerons`, now marking the
/// live shapes' symbol keys as it goes), so `WeakMap`s and `WeakSet`s
/// keyed by it lost them, and so did a chain of them (an entry's value an
/// object whose symbol key keys another's). What V8 and SpiderMonkey give.
#[test]
fn symbol_keys_keep_their_weak_entries() {
    let src = r#"
var obj = {}, wm = new WeakMap(), ws = new WeakSet(), wm2 = new WeakMap();
function setup() {
  var s = Symbol("s"), t = Symbol("t"), holder = {};
  obj[s] = 1;
  holder[t] = 2;
  wm.set(s, holder);
  ws.add(s);
  wm2.set(t, "deep");
}
function churn(n) { var a = []; for (var i = 0; i < n; i++) a.push({ i: i, s: "x" + i }); return a.length; }
setup();
churn(20000);
for (var i = 0; i < 3; i++) { churn(5000); $262.gc(); }
var s = Object.getOwnPropertySymbols(obj)[0];
var h = wm.get(s);
[String(s), ws.has(s), h && h[Object.getOwnPropertySymbols(h)[0]], h && wm2.get(Object.getOwnPropertySymbols(h)[0])].join(" ");
"#;
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("Symbol(s) true 2 deep"), "tier at {budget}");
    }
}

/// An atom a native took stays while the native runs (Codex on #40): it
/// may hold it across user code that collects again and again, while
/// nothing else names it, which freed it (its name then read panicking, or
/// its index another name's). `defineProperty` converts its key before
/// its descriptor's getters run, `JSON.parse` takes an object's keys
/// before its reviver deletes one. Both give what V8 and SpiderMonkey do,
/// with freed atoms poisoned too; the previous commit named the property
/// another name (`q3_2998`), or panicked poisoned. (The reviver's key it
/// got right by chance: a string of the name stayed reachable, which kept
/// its atom.)
#[test]
fn atoms_a_native_holds_outlive_its_callbacks() {
    let src = r#"
function churn() {
  for (var i = 0; i < 4; i++) {
    for (var j = 0; j < 3000; j++) { var t = {}; t["q" + i + "_" + j] = j; }
    $262.gc();
  }
}
var out = [];
var o = {};
var key = { toString() { return "fresh" + (1e6 + 7); } };
Object.defineProperty(o, key, { get value() { churn(); return 7; }, enumerable: true });
var k = Object.keys(o);
out.push(k.length + " " + k[0] + " " + o[k[0]]);
var seen = [];
JSON.parse('{"a":1,"zq' + (1e6 + 8) + '":2}', function (k, v) {
  if (k === "a") { delete this[Object.keys(this)[1]]; churn(); }
  seen.push(k);
  return v;
});
out.push(seen.join(","));
out.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    for poison in [false, true] {
        let mut vm = Vm::new();
        vm.set_atoms_poison(poison);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok("1 fresh1000007 7 | a,zq1000008,"), "poison {poison}");
    }
}

/// Natives and JS nested, each native holding an atom it took while the
/// JS it calls makes names and collects (the review of #40):
/// `defineProperty`'s key across its descriptor's getter, which runs
/// `JSON.parse`, whose reviver deletes a key it took and calls
/// `defineProperty` again, whose getter calls `map`, whose callback
/// collects. What V8 and SpiderMonkey give, at every tier, with freed
/// atoms poisoned too.
#[test]
fn atoms_held_by_nested_natives_survive_their_callbacks() {
    let src = r#"
function churn(tag) {
  for (var i = 0; i < 3; i++) {
    for (var j = 0; j < 2000; j++) { var t = {}; t["c" + tag + i + "_" + j] = j; }
    $262.gc();
  }
}
var out = [];
var o = {}, inner = {};
Object.defineProperty(o, { toString() { return "outer" + (1e6 + 1); } }, {
  get value() {
    JSON.parse('{"a":1,"jk' + (1e6 + 2) + '":2,"b":3}', function (k, v) {
      if (k === "a") {
        delete this[Object.keys(this)[1]];
        Object.defineProperty(inner, { toString() { return "inner" + (1e6 + 3); } },
          { get value() { churn("x"); return [3].map(function (n) { churn("m"); return n; })[0]; }, enumerable: true });
        churn("y");
      }
      out.push(k);
      return v;
    });
    churn("z");
    out.push(Object.keys(inner) + "=" + inner[Object.keys(inner)[0]]);
    return 1;
  },
  enumerable: true,
});
out.push(Object.keys(o) + "=" + o[Object.keys(o)[0]]);
out.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    for (budget, poison) in [(i64::MAX, false), (i64::MAX, true), (1, true), (2, true)] {
        let mut vm = Vm::new();
        vm.set_atoms_poison(poison);
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(
            r.as_deref(),
            Ok("a | jk1000002 | b |  | inner1000003=3 | outer1000001=1"),
            "tier at {budget}, poison {poison}"
        );
    }
}

/// An atom `Vm::intern` gave stays its name for good (Codex on #40): a
/// caller outside the VM may hold it across any collection, which freed it
/// once a marking found nothing naming it and it had gone unused (its name
/// then read panicking, or its index another name's). A name interned
/// before a program that makes and drops names never seen before, and
/// collects, is still its own, and a global property set through it after
/// is read by name.
#[test]
fn atoms_interned_outside_the_vm_stay() {
    let src = r#"
for (var r = 0; r < 4; r++) {
  for (var k = 0; k < 500; k++) { var o = {}; o["fresh" + r + "_" + k] = k; }
  $262.gc();
}
"#;
    let module = cloison::js::compile(src).unwrap();
    let read = cloison::js::compile("held_outside_the_vm").unwrap();
    for poison in [false, true] {
        let mut vm = Vm::new();
        vm.set_atoms_poison(poison);
        let a = vm.intern("held_outside_the_vm");
        vm.run(&module).unwrap();
        assert_eq!(vm.atom_name(a), "held_outside_the_vm", "poison {poison}");
        let g = vm.global_object();
        vm.set(g, a, cloison::value::Value::from_i32(7)).unwrap();
        assert_eq!(vm.run(&read).map(display).map_err(|e| e.to_string()).as_deref(), Ok("7"), "poison {poison}");
    }
}

/// An inlined function's `arguments` is an arguments object (#41): the
/// JIT made it an array of the call's arguments, with no `callee` and
/// `Array.isArray`, so a function calling itself through
/// `arguments.callee` threw once compiled. Its function is the frame's,
/// known or (inlined by its code) the one its guard checked; a strict
/// one's `callee` throws. Interpreted, compiled at once or not, and by
/// default: what V8 and SpiderMonkey give.
#[test]
fn inlined_frames_arguments_are_arguments_objects() {
    let src = r#"
var out = [];
function f(n) { var s = 0; for (var i = 0; i < 200; i++) s += i; return n <= 0 ? s : arguments.callee(n - 1) + 1; }
var t = 0;
for (var k = 0; k < 3000; k++) t += f(3);
out.push(t);
var fs = [];
for (var j = 0; j < 4; j++) fs.push((function (j) { return function (a) { return arguments.callee; }; })(j));
function run(f, k) { return f(k) === f; }
var seen = {};
for (var k = 0; k < 20000; k++) seen[run(fs[k % 4], k)] = 1;
out.push(Object.keys(seen).join());
function s(a) { "use strict"; return Object.getOwnPropertyNames(arguments).join(); }
function st(a) { "use strict"; try { arguments.callee; } catch (e) { return e instanceof TypeError; } }
function sl(a) { return Object.prototype.toString.call(arguments); }
function g(a) { return Array.isArray(arguments) + typeof arguments.push + arguments.length + arguments[1]; }
function h() {
  var o = {};
  for (var k = 0; k < 20000; k++) { o[s(1, 2)] = 1; o[st()] = 1; o[sl(k)] = 1; o[g(k, "x")] = 1; }
  return Object.keys(o).join(" ");
}
out.push(h());
out.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let want = "59709000 | true | 0,1,length,callee true [object Arguments] falseundefined2x";
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok(want), "tier at {budget}");
    }
    let r = Vm::new().run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok(want), "tier by default");
}

/// An inlined frame's `arguments` read only for its length and elements
/// stays the array of its call's arguments (`Lowering::array_arguments`),
/// and answers as the arguments object: a key that isn't an element it
/// has (`callee`, `push`, `-1`, past the end, a symbol, one an element of
/// `Array.prototype` names) leaves the compiled code, and the interpreter
/// reads an arguments object made at the exit (`materialize`), also
/// through a copy (`var a = arguments`). Interpreted, compiled at once or
/// not, and by default: what V8 and SpiderMonkey give.
#[test]
fn inlined_arguments_kept_arrays_answer_as_arguments_objects() {
    let src = r#"
var out = [];
function get() { var a = arguments; return a[a[0]]; }
var keys = [0, "callee", "push", "length", -1, 3, 1.5, "0", Symbol.iterator, "constructor", "toString", 1];
function len() { return arguments.length; }
function sum() { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s; }
function far() { return arguments[arguments[0] + 5]; }
var r = {};
for (var k = 0; k < 30000; k++) {
  var key = keys[k % keys.length];
  var v = get(key, "x");
  r[(typeof key === "symbol" ? "sym" : String(key)) + ":" + (typeof v === "function" ? "fn:" + (v === get) : String(v))] = 1;
  r["len" + len(1, 2, 3)] = 1;
  r["sum" + sum(1, 2, 3, k % 3)] = 1;
  r["far" + far(k % 2)] = 1;
}
out.push(Object.keys(r).sort().join(" "));
Array.prototype[7] = "a7";
Object.prototype[8] = "o8";
function at7() { return arguments[7]; }
function at8() { return arguments[8]; }
r = {};
for (var k = 0; k < 30000; k++) r[at7(1) + "/" + at8(1)] = 1;
out.push(Object.keys(r).join(" "));
delete Array.prototype[7];
delete Object.prototype[8];
function inner(a, b) { return a + b; }
function outer() { var a = arguments; return inner(a[0], a[1]) + a.length; }
var t = 0;
for (var k = 0; k < 30000; k++) t += outer(k, 1);
out.push(t);
function late() { return typeof arguments[arguments[0]]; }
r = {};
for (var k = 0; k < 30000; k++) r[late(k < 25000 ? 1 : "callee", 2)] = 1;
out.push(Object.keys(r).join());
out.join(" | ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let want = "-1:undefined 0:0 1.5:undefined 1:x 3:undefined callee:fn:true constructor:fn:false farundefined len3 length:2 push:undefined sum6 sum7 sum8 sym:fn:false toString:fn:false | undefined/o8 | 450075000 | number,function";
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok(want), "tier at {budget}");
    }
    let r = Vm::new().run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok(want), "tier by default");
}

/// A module run again in a VM loads its codes again, with the same
/// template sites: a site's strings array is the same for all of them (one
/// cache entry), and stays while any of them lives. Freeing the code that
/// made it forgot the entry another still used, whose next call then made
/// another array (Codex on #46). Three runs, the first two's functions
/// dropped and their codes freed.
#[test]
fn tagged_templates_shared_by_a_module_run_again_stay() {
    let module = cloison::js::compile(
        "var fs = globalThis.fs || (globalThis.fs = []); fs.push(function () { return (s => s)`x`; }); fs[fs.length - 1]();",
    )
    .unwrap();
    let drop = cloison::js::compile(
        "var saved = fs[2](); var same = fs[0]() === saved && fs[1]() === saved; fs[0] = fs[1] = null;",
    )
    .unwrap();
    let gc = cloison::js::compile("$262.gc(); $262.gc();").unwrap();
    let check = cloison::js::compile("same + ' ' + (fs[2]() === saved)").unwrap();
    let mut vm = Vm::new();
    for _ in 0..3 {
        vm.run(&module).unwrap();
    }
    vm.run(&drop).unwrap();
    let codes = vm.code_count();
    vm.run(&gc).unwrap();
    vm.run(&gc).unwrap();
    assert!(vm.code_count() < codes, "the first run's codes freed ({codes} before)");
    let r = vm.run(&check).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("true true"));
}

/// A tagged template's array stays its site's while it is reachable, even
/// once every code of the site is freed: the same module run again gives
/// it again (a reviewer's case on #47: the entry went with the last code,
/// so the next run made another array). Its codes freed, the array held by
/// a global only; the module run again.
#[test]
fn tagged_templates_held_outlive_their_codes() {
    let module = cloison::js::compile("globalThis.f = function (tag) { return tag`hello`; };").unwrap();
    let save = cloison::js::compile("var saved = f(s => s); f = undefined;").unwrap();
    let gc = cloison::js::compile("$262.gc(); $262.gc();").unwrap();
    let check =
        cloison::js::compile("f(s => s) === saved && Object.isFrozen(saved) && saved.raw[0] === 'hello'").unwrap();
    let other = cloison::js::compile("var g = function () { return 1; }; g();").unwrap();
    let mut vm = Vm::new();
    vm.run(&module).unwrap();
    vm.run(&save).unwrap();
    // (Other scripts run after it, so that its own is freed too.)
    for _ in 0..3 {
        vm.run(&other).unwrap();
    }
    let codes = vm.code_count();
    vm.run(&gc).unwrap();
    vm.run(&gc).unwrap();
    assert!(vm.code_count() < codes, "the first run's codes freed ({codes} before)");
    vm.run(&module).unwrap();
    let r = vm.run(&check).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("true"));
}

/// `Object.freeze(arguments)` edge cases (#14's follow-ups to #8):
/// duplicate parameters, parameters written after the freeze, deleted and
/// recreated indices, accessors on mapped and unmapped indices, an index
/// unmapped by a non-writable definition, an object escaped and frozen
/// from outside while its frame runs, freezing twice and after sealing,
/// writes in sloppy and strict code, and a hot function freezing its
/// `arguments` across collections. What SpiderMonkey and JavaScriptCore
/// give (V8 leaves an accessor at index 3 out of
/// `getOwnPropertyNames`), compiled at once or not.
#[test]
fn frozen_arguments_edge_cases() {
    let src = r#"
var out = [];
function j(o) { try { return JSON.stringify(Object.getOwnPropertyNames(o).map(function (k) { var d = Object.getOwnPropertyDescriptor(o, k); return k + ":" + ("value" in d ? (typeof d.value === "function" ? "fn" : String(d.value)) : "acc") + (d.writable ? "w" : "") + (d.configurable ? "c" : "") + (d.enumerable ? "e" : ""); })); } catch (e) { return "E:" + e.constructor.name; } }
// Duplicate parameter names (sloppy): the last one is mapped.
function dup(a, a) { Object.freeze(arguments); a = 9; return [arguments[0], arguments[1], a, Object.isFrozen(arguments)].join(); }
out.push(dup(1, 2));
// Frozen, then parameters written: the elements keep their values.
function wr(a, b) { Object.freeze(arguments); a = 5; b = 6; return [arguments[0], arguments[1], a, b].join(); }
out.push(wr(1, 2));
// Deleted and recreated indices, then frozen.
function del(a, b) { delete arguments[0]; arguments[0] = 7; a = 8; Object.freeze(arguments); return [arguments[0], a, arguments[1], j(arguments)].join("|"); }
out.push(del(1, 2));
// An accessor on a mapped index, and on one past the parameters.
function acc(a) { Object.defineProperty(arguments, "0", { get: function () { return "g"; }, configurable: true }); Object.defineProperty(arguments, "3", { get: function () { return "h"; }, configurable: true }); Object.freeze(arguments); a = 4; return [arguments[0], arguments[3], a, j(arguments)].join("|"); }
out.push(acc(1, 2));
// Unmapped by a non-writable definition, then frozen.
function unm(a, b) { Object.defineProperty(arguments, "1", { writable: false }); b = 3; Object.freeze(arguments); return [arguments[1], b].join(); }
out.push(unm(1, 2));
// Escaped while its frame runs, frozen from outside, then written inside.
var esc;
function outer(a) { esc = arguments; inner(); a = 10; return [esc[0], a, Object.isFrozen(esc)].join(); }
function inner() { Object.freeze(esc); }
out.push(outer(1));
// Frozen twice, and frozen after sealed.
function twice(a) { Object.freeze(arguments); Object.freeze(arguments); a = 2; return [arguments[0], a].join(); }
out.push(twice(1));
function sealed(a) { Object.seal(arguments); a = 2; var x = arguments[0]; Object.freeze(arguments); a = 3; return [x, arguments[0], a, Object.isSealed(arguments), Object.isFrozen(arguments)].join(); }
out.push(sealed(1));
// Writes to a frozen arguments object: ignored in sloppy, throwing in strict.
function sw(a) { Object.freeze(arguments); arguments[0] = 5; arguments[5] = 1; return [arguments[0], arguments.length, 5 in arguments].join(); }
out.push(sw(1));
function st(a) { "use strict"; Object.freeze(arguments); try { arguments[0] = 5; return "no"; } catch (e) { return e.constructor.name + arguments[0]; } }
out.push(st(1));
// Hot, many calls, with collections between: old-to-young references.
function hot(a, b) { Object.freeze(arguments); a = { v: b }; return arguments[0] + arguments[1].v; }
var s = 0;
for (var k = 0; k < 20000; k++) { s += hot(k % 7, { v: k % 3 }); if (k % 5000 == 0 && typeof $262 != "undefined") $262.gc(); }
out.push(s);
out.join(" ; ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let want = r#"1,2,9,true ; 1,2,5,6 ; 7|8|2|["0:7e","1:2e","length:2","callee:fn"] ; g|h|4|["0:acce","1:2e","3:acc","length:2","callee:fn"] ; 2,3 ; 1,10,true ; 1,2 ; 2,2,3,true,true ; 1,1,false ; TypeError1 ; 79996"#;
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok(want), "tier at {budget}");
    }
}

/// `+` converts its operands left to right, each once (ToPrimitive with
/// hint "default": `Symbol.toPrimitive`, else `valueOf` then `toString`),
/// a failed left conversion running nothing of the right's; then a string
/// or a number (or a BigInt) sum. Conversions that throw, return symbols,
/// objects or BigInts, allocate, reenter `+`, or split a surrogate pair;
/// each case interpreted, then hot, the same. Then whole concatenations
/// (appending and prepending, of strings, pairs, lone surrogates and
/// primitives) against `join`'s, not just their lengths (#14's follow-ups
/// to #9). What V8, SpiderMonkey and JavaScriptCore give.
#[test]
fn addition_converts_left_to_right_and_concatenates_whole() {
    let src = r#"
var out = [], log = [];
function P(name, v) { var o = {}; o[Symbol.toPrimitive] = function (h) { log.push(name + ":" + h); return v; }; return o; }
function V(name, v) { return { valueOf: function () { log.push(name + ".valueOf"); return v; }, toString: function () { log.push(name + ".toString"); return "s" + v; } }; }
function T(name) { return { valueOf: function () { log.push(name + ".throws"); throw new Error(name); } }; }
function run(f) { log = []; var r; try { r = String(f()); } catch (e) { r = "E:" + (e instanceof TypeError ? "TypeError" : e.message); } return r + "[" + log.join(",") + "]"; }
var cases = [
  function () { return P("a", 1) + P("b", 2); },
  function () { return P("a", "x") + 1; },
  function () { return 1 + P("b", "y"); },
  function () { return V("a", 3) + V("b", 4); },
  function () { return V("a", 3) + "z"; },
  function () { return T("a") + P("b", 2); },
  function () { return P("a", 1) + T("b"); },
  function () { var o = { get x() { log.push("get"); throw new Error("g"); } }; return o.x + P("b", 1); },
  function () { return new Date(0) + 1 === new Date(0).toString() + "1"; },
  function () { return P("a", Symbol.iterator) + ""; },
  function () { return P("a", {}) + 1; },
  function () { var big = P("a", 1n); return big + 2n; },
  function () { return P("a", 1n) + 2; },
  function () { var i = 0; var o = { valueOf: function () { log.push("v" + i); return i++; } }; return o + o + o; },
  function () { var s = ""; var o = { toString: function () { log.push("ts"); return "\ud83d"; } }; s = o + "\ude00"; return s.length + ":" + s.codePointAt(0).toString(16); },
  function () { var o = { valueOf: function () { log.push("alloc"); return new Array(1000).fill("q").join(""); } }; return (o + o).length; },
  function () { var o = { valueOf: function () { log.push("reenter"); return [1, 2] + [3]; } }; return o + 1; },
];
// Each case interpreted, then hot (compiled), and the same answers.
for (var c of cases) {
  var first = run(c), hot = first;
  for (var k = 0; k < 3000; k++) { var r = run(c); if (r !== first) hot = "DIFF " + r; }
  out.push(first + (hot === first ? "" : " " + hot));
}
// Whole concatenations, not just their lengths: ropes of both orders.
var a = "", b = "", parts = ["x", "😀", "é", "\ud800", "\udc00", "z", 7, true, null, undefined, 1.5];
for (var k = 0; k < 4000; k++) { var p = parts[k % parts.length]; a = a + p; b = p + b; }
var ea = [], eb = [];
for (var k = 0; k < 4000; k++) ea.push(String(parts[k % parts.length]));
for (var k = 3999; k >= 0; k--) eb.push(String(parts[k % parts.length]));
out.push(a === ea.join("") && b === eb.join("") ? "concat ok " + a.length : "concat BAD");
out.push([a.indexOf("😀"), a.lastIndexOf("\udc00"), a.charCodeAt(4000), b.slice(-20), (a + b).length].join(","));
out.join(" ; ");
"#;
    let module = cloison::js::compile(src).unwrap();
    let want = "3[a:default,b:default] ; x1[a:default] ; 1y[b:default] ; 7[a.valueOf,b.valueOf] ; 3z[a.valueOf] ; E:a[a.throws] ; E:b[a:default,b.throws] ; E:g[get] ; true[] ; E:TypeError[a:default] ; E:TypeError[a:default] ; 3[a:default] ; E:TypeError[a:default] ; 3[v0,v1,v2] ; 2:1f600[ts] ; 2000[alloc,alloc] ; 1,231[reenter] ; concat ok 10172 ; 1,10169,100,inednulltrue7z��é😀x,20344";
    for budget in [i64::MAX, 1, 2] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r.as_deref(), Ok(want), "tier at {budget}");
    }
}

#[test]
fn small_calls_under_register_pressure() {
    big_stack(small_calls_under_pressure);
}

fn small_calls_under_pressure() {
    // A function whose 150 variables leave about 100 registers: an
    // expression `d` deep holds `d` of them. A call of 0 to 2 arguments
    // holds its callee, its receiver and its arguments, as many as an
    // array of them would need, so it keeps them in registers to the
    // last one: as deep as `o.x` would go, less those it holds past one.
    let src = r#"
var vars = "";
for (var i = 0; i < 150; i++) vars += "var v" + i + " = " + i + ";";
function g() { return arguments.length; }
function C() { this.n = arguments.length; }
var o = { x: 1, g: g };
function deepest(s, n, w) {
    for (var d = 60; ; d++) {
        var e = s;
        for (var j = 0; j < d; j++) e = "(o.x + " + e + ")";
        var body = w ? "with (o) return " + e + ";" : "return " + e + ";";
        // (The inner function compiled when first called.)
        try { var r = Function("g", "o", "C", "return function () { " + vars + body + " };")(g, o, C)(1, 2, 3); }
        catch (x) { return d - 1; }
        if (r !== d + n) return "wrong at " + d;
    }
}
var base = deepest("o.x", 1), wbase = deepest("o.x", 1, true);
var calls = [["g()", 0, 0], ["g(1)", 1, 1], ["g(1, 2)", 2, 2], ["o.g()", 0, 1], ["o.g(1, 2)", 2, 3],
    ["new C().n", 0, 0], ["new C(1, 2).n", 2, 2], ["g.apply(o, arguments)", 3, 3],
    // (Past `SPARE_REGS`: in an array, which needs 3 registers.)
    ["g(" + "1,".repeat(30) + ")", 30, 2]];
var out = calls.map(function (c) { return base - deepest(c[0], c[1]) === c[2] ? "ok" : c[0] + ": " + (base - deepest(c[0], c[1])); });
// (Inside `with`: the lookup's registers, more than the arguments'.)
out.push(deepest("g()", 0, true) === deepest("g(1, 2)", 2, true) ? "ok" : "with");
out.join(" ");
"#;
    assert_eq!(value(src), ["ok"; 10].join(" "));
    // Not in an array either, just short of the registers.
    let vars: String = (0..150).map(|i| format!("var v{i} = {i};")).collect();
    let e = (0..95).fold("o.g(1, 2)".to_string(), |e, _| format!("(o.x + {e})"));
    let code = cloison::js::compile(&format!("function h(o) {{ {vars} return {e}; }}")).unwrap();
    let text = cloison::asm::disassemble(&code);
    assert!(!text.contains("call_spread") && !text.contains("CreateArray"), "{text}");
}

#[test]
fn short_templates_under_register_pressure() {
    big_stack(short_templates_under_pressure);
}

fn short_templates_under_pressure() {
    // A template of one substitution, with few registers left: one
    // `template` intrinsic joining its pieces, as with registers to
    // spare, not one per piece.
    let vars: String = (0..150).map(|i| format!("var v{i} = {i};")).collect();
    let e = (0..96).fold("`a${o.x}b`".to_string(), |e, _| format!("(o.x + {e})"));
    let src = format!("function h(o) {{ {vars} return {e}; }}");
    let text = cloison::asm::disassemble(&cloison::js::compile(&src).unwrap());
    // (The only intrinsic there.)
    assert_eq!(text.matches("Intrinsic ").count(), 1, "{text}");
    assert_eq!(value(&format!("{src} h({{ x: 1 }})")), format!("{}a1b", "1".repeat(96)));
}

#[test]
fn calls_with_only_eval_scopes_are_plain() {
    // A call of a name a direct `eval` may declare, in no `with`: looked
    // up in the eval's scope object, but never a method call, so one
    // `CallUndefined` and no `Call` checking a flag.
    let src = "function f() { eval(''); return h(1); } \
        function k(o) { with (o) return h(1); } \
        function h(a) { 'use strict'; return [this, a].join(); } \
        function m(o) { eval(''); return h(...[1]); } \
        [f(), k({}), k({ h: h, toString: () => 'o' }), m(), \
         (function () { eval('var h = function (a) { return this === globalThis; }'); return h(2); })()].join(' ')";
    assert_eq!(value(src), ",1 ,1 o,1 ,1 true");
    let text = cloison::asm::disassemble(&cloison::js::compile(src).unwrap());
    let body = |name: &str| {
        let start = text.find(&format!("function {name}")).unwrap_or_else(|| panic!("{text}"));
        let rest = &text[start..];
        rest[..rest[1..].find("function ").map_or(rest.len(), |i| i + 1)].to_string()
    };
    let calls = |b: &str| (b.matches("\n        Call ").count(), b.matches("CallUndefined ").count());
    assert_eq!(calls(&body("f")), (0, 1), "{}", body("f"));
    assert_eq!(calls(&body("k")), (1, 1), "{}", body("k"));
}

#[test]
fn long_tagged_templates_make_their_arrays_once() {
    // A long tagged template's strings go to `template_object` in two
    // arrays: made only when the site has no strings array yet, after
    // `[site]` alone found none.
    let src = format!(
        "function tag(s) {{ return s; }} function k() {{ return tag`{}`; }} var a = k(); a === k() && a.length + a.raw[1]",
        "${0}\\x".repeat(300)
    );
    assert_eq!(value(&src), "301\\x");
    let text = cloison::asm::disassemble(&cloison::js::compile(&src).unwrap());
    // (`tag`'s argument array before: `[site]`, a jump past the arrays.)
    let k = &text[text.find("function k").unwrap()..];
    let lines: Vec<&str> = k.lines().map(str::trim).collect();
    let jump = lines.iter().position(|l| l.starts_with("JumpIfToBooleanTrue")).expect(k);
    assert!(lines[jump - 1].starts_with("Intrinsic") && lines[jump - 1].ends_with(", 1"), "{k}");
    assert_eq!(lines[jump..].iter().filter(|l| l.starts_with("CreateArray")).count(), 2, "{k}");
}

#[test]
fn forwarded_arguments_under_register_pressure() {
    // `g.apply(x, arguments)` in a function that makes no `arguments`
    // object (it only passes them on), with few registers left: still
    // `CallApplyArguments`, not a call with its arguments in an array.
    let src = r#"
var out = [];
function g(a) { return a; }
var o = { x: 0 };
var vars = "";
for (var i = 0; i < 150; i++) vars += "var v" + i + " = " + i + ";";
var e = "g.apply(this, arguments)";
for (var j = 0; j < 90; j++) e = "(o.x + " + e + ")";
out.push(Function("g", "o", "return function () { " + vars + " return " + e + "; };")(g, o)(42));
function sink() { return arguments[arguments.length - 1]; }
for (var k = 230; k < 240; k++) {
    var h = Function("sink", "g", "return function () { return sink(" + "0,".repeat(k) + " g.apply(null, arguments)); };")(sink, g);
    out.push(h(k));
}
out.join(" ");
"#;
    let ks: Vec<String> = (230..240).map(|k| k.to_string()).collect();
    assert_eq!(big_stack(|| value(src)), format!("42 {}", ks.join(" ")));
}

/// `Vm::string_units` reads a cons string without allocating (#69): with
/// another VM run and dropped since, making it flat would have allocated
/// through the stale current VM, and two of its slices are read at once.
#[test]
fn string_units_reads_cons_strings_in_place() {
    let mut b = Vm::new();
    let s = b.run(&cloison::js::compile("'x'.repeat(100) + 'y'").unwrap()).unwrap();
    let t = b.run(&cloison::js::compile("'z' + 'w'.repeat(50)").unwrap()).unwrap();
    {
        let mut a = Vm::new();
        a.run(&cloison::js::compile("0").unwrap()).unwrap();
    }
    let (u, w) = (b.string_units(s).unwrap(), b.string_units(t).unwrap());
    assert_eq!((u.len(), u[0], u[100]), (101, b'x' as u16, b'y' as u16));
    assert_eq!((w.len(), w[0], w[50]), (51, b'z' as u16, b'w' as u16));
    assert!(b.string_units(cloison::value::Value::from_i32(1)).is_none());
}

/// A VM is the running one only while it runs (issue #70): a string of
/// one kept by the host is made flat by none other, even after another VM
/// ran and was dropped (it was flattened into that VM's freed heap).
#[test]
fn values_outlive_another_vm_run() {
    let mut b = Vm::new();
    let s = b.run(&cloison::js::compile("var s = 'x'.repeat(100) + 'y'.repeat(100); s").unwrap()).unwrap();
    {
        let mut a = Vm::new();
        a.run(&cloison::js::compile("0").unwrap()).unwrap();
    }
    assert_eq!(display(s).len(), 200);
    let mut a = Vm::new();
    a.run(&cloison::js::compile("0").unwrap()).unwrap();
    let m = cloison::js::compile("s.slice(99, 101) + s.length").unwrap();
    assert_eq!(display(b.run(&m).unwrap()), "xy200");
    drop(a);
    assert_eq!(display(b.run(&m).unwrap()), "xy200");
    assert_eq!(display(s), "x".repeat(100) + &"y".repeat(100));
    // The message of what a run threw is made after it, as one.
    let e = b.run(&cloison::js::compile("throw new Error('m' + s)").unwrap()).unwrap_err();
    assert_eq!(b.uncaught_message(&e), format!("Error: m{}", display(s)));
    // A host's property access between runs makes a cons string key flat.
    let o = b.run(&cloison::js::compile("var o = {}; o[s + 'z'] = 7; o").unwrap()).unwrap();
    let k = b.run(&cloison::js::compile("s + 'z'").unwrap()).unwrap();
    assert_eq!(display(b.get_keyed(o, k).unwrap()), "7");
}

/// A host's operations between runs enter the VM as a run does (#70): a
/// JS getter, setter or trap runs, a key's `toString` too, what they throw
/// is the operation's error (and none is left pending for the next), and
/// a native called from the host calls JS.
#[test]
fn host_operations_between_runs_run_js() {
    let mut vm = Vm::new();
    let src = r#"
var log = [];
var o = { k: 1, a: 5, get g() { return "G" + this.k; }, set s(v) { log.push("set " + v); } };
var key = { toString() { return "a"; } };
var bad = { toString() { throw new Error("boom"); } };
var p = new Proxy({}, { get(t, k) { return "P" + String(k); }, has(t, k) { return k === "yes"; },
  ownKeys() { throw new TypeError("no keys"); } });
var map = Array.prototype.map, xs = [1, 2, 3], twice = x => x * 2;
var C = function () {}; Object.defineProperty(C, Symbol.hasInstance, { value: v => v === 7 });
0"#;
    vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    let g = vm.global_object();
    let get = |vm: &mut Vm, name: &str| {
        let a = vm.intern(name);
        vm.get(g, a).unwrap()
    };
    let o = get(&mut vm, "o");
    let gname = vm.intern("g");
    assert_eq!(display(vm.get(o, gname).unwrap()), "G1");
    let sname = vm.intern("s");
    vm.set(o, sname, cloison::value::Value::from_i32(3)).unwrap();
    let key = get(&mut vm, "key");
    assert_eq!(display(vm.get_keyed(o, key).unwrap()), "5");
    let bad = get(&mut vm, "bad");
    let e = vm.get_keyed(o, bad).unwrap_err();
    assert_eq!(vm.uncaught_message(&e), "Error: boom");
    let e = vm.set_keyed(o, bad, cloison::value::Value::from_i32(1)).unwrap_err();
    assert_eq!(vm.uncaught_message(&e), "Error: boom");
    // Nothing left pending: the next conversion runs.
    assert_eq!(display(vm.get_keyed(o, key).unwrap()), "5");
    let p = get(&mut vm, "p");
    let x = vm.intern("x");
    assert_eq!(display(vm.get(p, x).unwrap()), "Px");
    let yes = vm.new_string(&"yes".encode_utf16().collect::<Vec<u16>>());
    assert!(vm.has_property(p, yes).unwrap());
    let e = vm.for_in_keys(p).unwrap_err();
    assert_eq!(vm.uncaught_message(&e), "TypeError: no keys");
    let (map, xs, twice) = (get(&mut vm, "map"), get(&mut vm, "xs"), get(&mut vm, "twice"));
    let r = vm.call_other(map, xs, &[twice]).unwrap();
    let join = vm.intern("join");
    let join = vm.get(r, join).unwrap();
    assert_eq!(display(vm.call_other(join, r, &[]).unwrap()), "2,4,6");
    let c = get(&mut vm, "C");
    assert!(vm.instance_of(cloison::value::Value::from_i32(7), c).unwrap());
    assert!(!vm.instance_of(cloison::value::Value::from_i32(8), c).unwrap());
    let r = vm.run(&cloison::js::compile("log.join()").unwrap()).unwrap();
    assert_eq!(display(r), "set 3");
}

/// A value of one VM given to another's operation (#70): a cons string key
/// can't be made flat by a VM that doesn't own it, which panics, and both
/// VMs work after.
#[test]
fn another_vms_cons_key_panics_cleanly() {
    let mut a = Vm::new();
    let mut b = Vm::new();
    let xy = "var xy = 'x'.repeat(40) + 'y'.repeat(40);";
    let o = a.run(&cloison::js::compile(&format!("{xy} var o = {{ [xy]: 1 }}; o")).unwrap()).unwrap();
    let k = b.run(&cloison::js::compile(&format!("{xy} xy")).unwrap()).unwrap();
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| a.get_keyed(o, k)));
    assert!(r.is_err());
    assert_eq!(display(b.get_keyed(b.global_object(), k).unwrap()), "undefined");
    let ka = a.run(&cloison::js::compile("'x'.repeat(40) + 'y'.repeat(40)").unwrap()).unwrap();
    assert_eq!(display(a.get_keyed(o, ka).unwrap()), "1");
    assert_eq!(display(b.run(&cloison::js::compile("xy.length").unwrap()).unwrap()), "80");
}

/// `Global`s made, cloned and dropped over many runs (the review of
/// #162): of young cells nothing else holds, through minor collections,
/// full ones and concurrent cycles moving cells (the last checking no
/// reference to a moved cell is left), each still reading its object;
/// slots freed and taken again; and past their VM, which drops with them
/// alive: they clone and drop then, and are no later VM's.
#[test]
fn globals_survive_clones_drops_and_moving_collections() {
    use cloison::vm::Global;
    let run = |vm: &mut Vm, src: &str| vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    for config in 0..3 {
        let mut vm = Vm::new();
        match config {
            0 => {}
            1 => vm.set_gc_evacuate_all(),
            _ => vm.set_gc_evacuate_cycles(),
        }
        let (n, s) = (vm.intern("n"), vm.intern("s"));
        let mut held: Vec<(Global, i32)> = Vec::new();
        let mut seed = 12345u32;
        let mut next = |m: usize| {
            seed = seed.wrapping_mul(1103515245).wrapping_add(12345);
            (seed >> 16) as usize % m
        };
        for round in 0..30 {
            let a = run(
                &mut vm,
                &format!(
                    "(function () {{ var a = []; for (var i = 0; i < 40; i++) a.push({{n: {round} * 100 + i, s: 'v' + i}}); return a; }})()"
                ),
            );
            for i in 0..40 {
                let v = vm.get_keyed(a, cloison::value::Value::from_i32(i)).unwrap();
                held.push((Global::new(&vm, v), round * 100 + i));
            }
            for _ in 0..15 {
                let k = next(held.len());
                let (g, id) = (held[k].0.clone(), held[k].1);
                held.push((g, id));
            }
            for _ in 0..30 {
                let k = next(held.len());
                held.swap_remove(k);
            }
            let full = if round % 3 == 0 { "$262.gc();" } else { "" };
            run(&mut vm, &format!("for (var i = 0; i < 3e4; i++) var x = {{i: [i]}}; {full}"));
            for (g, id) in &held {
                let o = g.get(&vm);
                assert_eq!(display(vm.get(o, n).unwrap()), id.to_string(), "config {config}, round {round}");
                assert_eq!(display(vm.get(o, s).unwrap()), format!("v{}", id % 100));
            }
        }
        vm.verify_heap();
        let left: Vec<Global> = held.into_iter().map(|(g, _)| g).collect();
        assert!(left.len() > 300);
        drop(vm);
        let copies: Vec<Global> = left.iter().map(Global::clone).collect();
        let later = Vm::new();
        assert!(!copies.iter().chain(&left).any(|g| g.is_of(&later)));
        drop(left);
        drop(copies);
    }
}

/// A value the host keeps across runs as a `Global` (#142): a root, kept
/// through collections that free what nothing else holds and updated
/// as they move its cell; of its own VM only; its slot free once
/// dropped. (The raw `Value` of the issue's reproducer read `undefined`
/// after the second run.)
#[test]
fn globals_keep_host_values_across_runs() {
    use cloison::vm::Global;
    let mut vm = Vm::new();
    vm.set_gc_evacuate_all();
    let run = |vm: &mut Vm, src: &str| vm.run(&cloison::js::compile(src).unwrap()).unwrap();
    let o = run(&mut vm, "({k: 'v'.repeat(3), a: [1, 2, 3]})");
    let (at, o) = (o.to_bits(), Global::new(&vm, o));
    let copy = o.clone();
    let e = run(&mut vm, "({})");
    let gone = Global::new(&vm, e);
    drop(gone);
    for _ in 0..3 {
        run(&mut vm, "for (var i = 0; i < 2e5; i++) var x = {i: [i]}; $262.gc(); $262.gc();");
    }
    let k = vm.intern("k");
    assert_eq!(display(vm.get(o.get(&vm), k).unwrap()), "vvv");
    assert_eq!(o.get(&vm), copy.get(&vm));
    assert_ne!(o.get(&vm).to_bits(), at, "evacuation moved it, and its Global with it");
    let other = Vm::new();
    assert!(o.is_of(&vm) && !o.is_of(&other));
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| o.get(&other)));
    assert!(r.is_err());
    let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| Global::new(&other, o.get(&vm))));
    assert!(r.is_err());
}

/// #163's records (an index mapped after the return, to an upvalue's cell
/// or to a scope object's property) while every collection moves cells:
/// both links survive, the cells found where they moved. What
/// SpiderMonkey and JavaScriptCore give.
#[test]
fn mapped_arguments_after_a_return_survive_moving_collections() {
    let src = r#"
var gc = typeof $262 !== "undefined" ? $262.gc : function () {};
function f(a, b) { return [function (v) { if (v !== undefined) a = v; return a; }, arguments]; }
function e(a) { eval(""); return [function (v) { if (v !== undefined) a = v; return a; }, arguments]; }
var keep = [];
for (var i = 0; i < 300; i++) keep.push(f({ i: i }, i), e({ i: i }));
function churn() { for (var r = 0; r < 2; r++) { var junk = []; for (var j = 0; j < 20000; j++) junk.push({ j: j, s: "x" + j }); gc(); } }
churn();
var bad = 0;
for (var i = 0; i < keep.length; i++) {
  var k = keep[i];
  if (k[1][0] !== k[0]() || k[0]().i !== i >> 1) bad++;
  k[1][0] = { n: i };
}
churn();
for (var i = 0; i < keep.length; i++) {
  var k = keep[i];
  if (k[0]().n !== i) bad++;
  k[0]({ m: "s" + i });
}
churn();
for (var i = 0; i < keep.length; i++) if (keep[i][1][0].m !== "s" + i) bad++;
bad + " " + keep.length
"#;
    let module = cloison::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_gc_evacuate_all();
    vm.set_gc_verify(256 << 10);
    let r = vm.run(&module).map(display).map_err(|e| e.to_string());
    assert_eq!(r.as_deref(), Ok("0 600"));
    vm.verify_heap();
}

/// A sloppy generator's or async function's `arguments` is mapped to its
/// simple parameters, as any sloppy function's (#177; ECMA-262
/// FunctionDeclarationInstantiation step 22): both ways, before and after
/// a `yield` or an `await` (the frame gone, the parameters in the scope
/// object), from a closure, after a delete, let out of the generator,
/// with an index past the arguments, with a direct `eval`, frozen, many
/// at once; a strict one isn't. What SpiderMonkey and JavaScriptCore give.
#[test]
fn generators_and_async_functions_map_arguments() {
    let src = r#"
var out = [];
function* g(a, b) {
  out.push(arguments[0] + "," + b);
  a = 5;
  out.push(arguments[0]);
  var x = yield a;
  out.push(arguments[0] + "," + arguments[1] + "," + x);
  arguments[1] = "B";
  out.push(b);
  yield* [1];
  var k = function () { a = "c"; };
  k();
  out.push(arguments[0]);
  delete arguments[0];
  a = "d";
  out.push(String(arguments[0]));
  return arguments.length;
}
var it = g(1, 2);
out.push(JSON.stringify(it.next()), JSON.stringify(it.next("X")), JSON.stringify(it.next()), JSON.stringify(it.next()));
function* esc(a) { yield arguments; yield a; }
var e = esc(1); e.next().value[0] = 42; out.push(e.next().value);
function* one(a, b) { b = 3; yield arguments.length + ":" + arguments[1]; } out.push(one(1).next().value);
function* ge(a) { eval("arguments[0] = 8"); yield a; eval("a = 11"); yield arguments[0]; }
var gi = ge(1); out.push(gi.next().value, gi.next().value);
function* fr(a) { Object.freeze(arguments); a = 2; yield arguments[0]; } out.push(fr(1).next().value);
function* st(a) { "use strict"; arguments[0] = 2; yield a; } out.push(st(1).next().value);
var gens = [];
for (var i = 0; i < 100; i++) gens.push((function* (a) { while (true) { a++; yield arguments[0]; } })(i));
var sum = 0;
for (var r = 0; r < 3; r++) for (var i = 0; i < 100; i++) sum += gens[i].next().value;
out.push(sum);
out.join(" ")
"#;
    assert_eq!(
        value(src),
        r#"1,2 5 5,2,X B c undefined {"value":5,"done":false} {"value":1,"done":false} {"value":2,"done":true} {"done":true} 42 1:undefined 8 11 1 1 15450"#
    );
    let src = r#"
var log = [];
async function h(a, b) { arguments[0] = 3; log.push(a); await null; a = 4; log.push(arguments[0]); await null; arguments[1] = 9; log.push(b); return arguments[0]; }
async function he(a) { eval("a = 7"); await 0; return arguments[0]; }
async function hs(a) { "use strict"; arguments[0] = 2; await 0; return a; }
Promise.all([h(1, 2), he(1), hs(1)]).then(function (v) { globalThis.out = log.join(",") + " " + v.join(","); });
"#;
    assert_eq!(out_after_jobs(src), "3,4,9 4,7,1");
}

/// A named function expression's own name, shadowed by a parameter, a
/// `var`, a function declaration or (named `arguments`) the `arguments`
/// object, is not the binding: those are the function's own (ECMA-262
/// InstantiateOrdinaryFunctionExpression puts the name in an environment
/// around them). Its scope object's layout left a generator's parameter
/// of the name out by its spelling, so the mapping to it was lost at the
/// first `yield`; and a function named `arguments` had its `arguments`
/// overwritten with itself. Both ways across `yield` and `await`: in
/// generators, async functions and async generators, with a direct
/// `eval`, duplicate parameters, a computed method name. What
/// SpiderMonkey and V8 give (JavaScriptCore and QuickJS keep the
/// function declaration's value over the write in `g8`).
#[test]
fn a_parameter_shadowing_a_function_expressions_name_stays_mapped() {
    let src = r#"
var out = [];
var g1 = function* f(f) { var a = arguments; yield a; yield f; };
var i1 = g1(1); i1.next().value[0] = 2; out.push(i1.next().value);
var g2 = function* f(f) { var a = arguments; yield a; f = 5; yield a[0]; };
var i2 = g2(1); i2.next(); out.push(i2.next().value);
var g5 = function* f(f) { eval(""); var a = arguments; yield a; yield f; };
var i5 = g5(1); i5.next().value[0] = 6; out.push(i5.next().value);
var g6 = function* f(f, f) { var a = arguments; yield a; yield f; };
var i6 = g6(1, 2); var a6 = i6.next().value; a6[1] = 7; a6[0] = 8; out.push(i6.next().value);
var g7 = function* f(f) { var f; var a = arguments; yield a; yield f; };
var i7 = g7(1); i7.next().value[0] = 10; out.push(i7.next().value);
var g8 = function* f(f) { function f() {} var a = arguments; yield a; yield typeof f; };
var i8 = g8(1); i8.next().value[0] = 11; out.push(i8.next().value);
var g9 = function* arguments(x) { yield arguments; yield x; };
var i9 = g9(1); i9.next().value[0] = 12; out.push(i9.next().value);
var o = { *["m" + 1](m1) { var a = arguments; yield a; yield m1; } };
var i10 = o.m1(1); i10.next().value[0] = 13; out.push(i10.next().value);
var g11 = function* f(x) { yield typeof f; f = 1; yield typeof f; };
var i11 = g11(1); out.push(i11.next().value, i11.next().value);
out.push((function arguments(x) { return typeof arguments; })(1));
out.push((function arguments(x) { arguments[0] = 2; var k = () => x; return k(); })(1));
out.push((function f(f) { var k = () => f; function f() {} arguments[0] = 11; return typeof k(); })(1));
out.push((function f(f) { eval("f = 4"); return arguments[0]; })(1));
out.join(" ")
"#;
    assert_eq!(value(src), "2 5 6 7 10 number 12 13 function function object 2 number 4");
    let src = r#"
var log = [];
var h = async function f(f) { var a = arguments; await 0; a[0] = 3; await 0; return f; };
var h2 = async function* f(f) { var a = arguments; yield 0; f = 9; yield a[0]; };
var i = h2(1);
var o = { async *["c" + "m"](cm) { var a = arguments; yield 0; cm = 6; yield a[0]; } };
var j = o.cm(1);
Promise.all([h(1), i.next().then(function () { return i.next(); }), j.next().then(function () { return j.next(); })])
  .then(function (v) { globalThis.out = [v[0], v[1].value, v[2].value].join(" "); });
"#;
    assert_eq!(out_after_jobs(src), "3 9 6");
}

/// What stores a parameter as no expression does maps `arguments` too: a
/// function declaration of its name in the body (FunctionDeclaration-
/// Instantiation stores it in the parameter's binding, ECMA-262 10.2.11
/// steps 28 and 36), a `var` of it with an initializer, a `for`-`in` or
/// `for`-`of` target of it, `var` or not. The object was left unmapped,
/// its elements the arguments. One in a block isn't Annex B's for a
/// parameter's name (B.3.2.1); a `catch` binding's `var` stores the
/// binding. In plain functions, generators, async functions and async
/// generators, with `eval` and duplicates. What SpiderMonkey and V8 give
/// (JavaScriptCore and QuickJS keep the argument for a declaration).
#[test]
fn declarations_storing_a_parameter_map_arguments() {
    let src = r#"
var out = [];
function a1(f) { function f() {} return typeof arguments[0]; } out.push(a1(1));
function a2(f) { var f = 2; return arguments[0]; } out.push(a2(1));
function a3(f) { for (f in { x: 1 }); return arguments[0]; } out.push(a3(1));
function a4(f) { for (var f in { y: 1 }); return arguments[0]; } out.push(a4(1));
function a5(f) { for (f of [5]); return arguments[0]; } out.push(a5(1));
function a6(f) { for (var f of [6]); return arguments[0]; } out.push(a6(1));
function a7(f) { { function f() {} } return typeof arguments[0]; } out.push(a7(1));
function a8(f) { for (var f = 8; false;); return arguments[0]; } out.push(a8(1));
function a9(f, f) { function f() {} return typeof arguments[0] + typeof arguments[1]; } out.push(a9(1, 2));
function a10(f) { eval(""); function f() {} return typeof arguments[0]; } out.push(a10(1));
function a11(f) { try { throw 3 } catch (f) { var f = 11; } return arguments[0]; } out.push(a11(1));
function* g1(f) { function f() {} yield typeof arguments[0]; } out.push(g1(1).next().value);
function* g2(f) { var f = 2; yield 0; yield arguments[0]; } var i2 = g2(1); i2.next(); out.push(i2.next().value);
var fe = function* f(f) { function f() {} yield typeof arguments[0]; }; out.push(fe(1).next().value);
function a12(f) { function f() {} arguments[0] = 11; return typeof f; } out.push(a12(1));
out.join(" ")
"#;
    assert_eq!(value(src), "function 2 x y 5 6 number 8 numberfunction function 1 function 2 function number");
    let src = r#"
async function h1(f) { function f() {} await 0; return typeof arguments[0]; }
async function* h2(f) { function f() {} yield 0; yield typeof arguments[0]; }
var i = h2(1);
Promise.all([h1(1), i.next().then(function () { return i.next(); })])
  .then(function (v) { globalThis.out = v[0] + " " + v[1].value; });
"#;
    assert_eq!(out_after_jobs(src), "function function");
}

/// Every form that stores a parameter, or an element of `arguments`, maps
/// the object (`aliases_arguments`, `stores_parameter`, which now name
/// each expression and statement form): `||=`, `&&=` and `??=` were
/// taken for reads (#215's review), and the operand of `await` wasn't
/// looked at. Also compound assignments, destructuring targets (nested,
/// defaults, rest), `for`-`of` heads with patterns, parenthesized targets,
/// `catch` patterns, `switch`, `with`, computed keys, optional chains;
/// `delete` and `typeof` of a parameter store nothing, a `catch` binding
/// is its own. Both ways, in plain functions, generators, async functions
/// and async generators. What SpiderMonkey and V8 give.
#[test]
fn every_store_form_maps_arguments() {
    let src = r#"
var out = [];
function* g1(a) { a ||= 7; yield arguments[0]; } out.push(g1(0).next().value);
function* g2(a) { arguments[0] ||= 7; yield a; } out.push(g2(0).next().value);
function l1(a) { a &&= 8; return arguments[0]; } out.push(l1(1));
function l2(a) { a ??= 9; return arguments[0]; } out.push(l2(null));
function l3(a) { arguments[0] ??= 9; return a; } out.push(l3(undefined));
function c1(a) { a += 1; return arguments[0]; } out.push(c1(1));
function c2(a) { a **= 3; return arguments[0]; } out.push(c2(2));
function c3(a) { arguments[0] <<= 2; return a; } out.push(c3(1));
function d1(a) { [a] = [11]; return arguments[0]; } out.push(d1(1));
function d2(a, b) { ({ x: { y: a = 12 } } = { x: {} }); return arguments[0]; } out.push(d2(1));
function d3(a) { [, ...a] = [0, 13]; return arguments[0].join(); } out.push(d3(1));
function d4(a) { for ([a] of [[14]]); return arguments[0]; } out.push(d4(1));
function d5(a) { for ({ a } of [{ a: 15 }]); return arguments[0]; } out.push(d5(1));
function d6(a) { for (var [a] of [[16]]); return arguments[0]; } out.push(d6(1));
function d7(a) { for (let [x = (a = 17)] of [[]]); return arguments[0]; } out.push(d7(1));
function u1(a) { (a)++; return arguments[0]; } out.push(u1(1));
function u2(a) { ((a)) = 19; return arguments[0]; } out.push(u2(1));
function t1(a) { delete a; typeof a; return arguments[0]; } out.push(t1(20));
function k1(a) { try { throw [21] } catch ([a]) { a = 0; } return arguments[0]; } out.push(k1(1));
function k2(a) { try { throw {} } catch ({ x = (a = 22) }) {} return arguments[0]; } out.push(k2(1));
function s1(a) { lbl: switch (1) { case 1: a = 23; break lbl; } return arguments[0]; } out.push(s1(1));
function w1(a) { with ({}) { a = 24; } return arguments[0]; } out.push(w1(1));
function w2(a) { with ({ a: 0 }) { a = 25; } return arguments[0]; } out.push(w2(1));
function cl(a) { class C { [a = 26]() {} } return arguments[0]; } out.push(cl(1));
function cf(a) { var o = { [a = 27]: 1 }; return arguments[0]; } out.push(cf(1));
function aw1(a) { return (async () => { await (a = 28); })().then(() => 0), arguments[0]; } out.push(aw1(1));
function y1(a) { var it = (function* () {})(); return `${a = 29}` && arguments[0]; } out.push(y1(1));
function ch(a) { var o = null; o?.[a = 30]; return arguments[0]; } out.push(ch(1));
function ch2(a) { var o = {}; o?.[a = 31]; return arguments[0]; } out.push(ch2(1));
function cm(a) { a ||= (arguments[0] = 32); return arguments[0] + "," + a; } out.push(cm(0));
out.join(" ")
"#;
    assert_eq!(value(src), "7 7 8 9 9 2 8 4 11 12 13 14 15 16 17 2 19 20 1 22 23 24 1 26 27 28 29 1 31 32,32");
    let src = r#"var log = [];
async function h1(a) { await 0; a ||= 33; return arguments[0]; }
async function h2(a) { await 0; arguments[0] ??= 34; await 0; return a; }
async function* h3(a) { yield 0; a &&= 35; yield arguments[0]; }
var i3 = h3(1);
Promise.all([h1(0), h2(null), i3.next().then(() => i3.next())]).then(v => { globalThis.out = [v[0], v[1], v[2].value].join(" "); });
"#;
    assert_eq!(out_after_jobs(src), "33 34 35");
}

/// Every assignment target position classified by one function
/// (`codegen::target_aliases`), from expressions and statements alike: a
/// member of `arguments` as a `for`-`of` or `for`-`in` head, in array and
/// object patterns at any depth, with defaults, rest and computed keys,
/// parenthesized, with a computed key, updated; a parameter in `for`
/// heads' patterns. A `for (arguments[0] of …)` head was taken for a
/// read and the object left unmapped (#215's review). Both ways, in plain
/// functions, generators, async functions and async generators. What
/// SpiderMonkey and V8 give.
#[test]
fn every_target_position_maps_arguments() {
    let src = r#"
var out = [];
function t1(a) { for (arguments[0] of [7]) {} return a; } out.push(t1(0));
function t2(a) { for (arguments[0] in { k: 1 }) {} return a; } out.push(t2(0));
function t3(a) { for ([arguments[0]] of [[8]]) {} return a; } out.push(t3(0));
function t4(a) { for ({ x: arguments[0] } of [{ x: 9 }]) {} return a; } out.push(t4(0));
function t5(a) { [arguments[0]] = [10]; return a; } out.push(t5(0));
function t6(a) { ({ a: arguments[0] } = { a: 11 }); return a; } out.push(t6(0));
function t7(a) { [...arguments[0]] = [12]; return a.join(); } out.push(t7(0));
function t8(a) { [[{ y: [arguments[0] = 13] }]] = [[{ y: [] }]]; return a; } out.push(t8(0));
function t9(a, i) { arguments[i] = 14; return a; } out.push(t9(0, 0));
function t10(a, i) { arguments[i + 0]++; return a; } out.push(t10(14, 0));
function t11(a) { (arguments[0]) = 16; return a; } out.push(t11(0));
function t12(a) { ++(arguments[0]); return a; } out.push(t12(16));
function t13(a) { ({ ...arguments[0] } = { q: 18 }); return a.q; } out.push(t13(0));
function t14(a) { ({ [(a = 19, "z")]: arguments[1] } = {}); return arguments[0]; } out.push(t14(0, 0));
function t15(a) { for ([a] of [[20]]); return arguments[0]; } out.push(t15(0));
function t16(a) { for ({ a = 21 } of [{}]); return arguments[0]; } out.push(t16(0));
function t17(a) { for (a in { z: 1 }); return arguments[0]; } out.push(t17(0));
function t18(a) { for (let [x = (arguments[0] = 22)] of [[]]); return a; } out.push(t18(0));
function t19(a) { arguments.length = 0; return a + "," + arguments.length; } out.push(t19(23));
function t20(a) { [arguments[0], a] = [24, 25]; return arguments[0] + "," + a; } out.push(t20(0));
function t21(a) { ((a)) &&= 26; return arguments[0]; } out.push(t21(1));
function t22(a) { for (arguments[0] of [27]) {} return typeof arguments[0] + a; } out.push(t22(0));
function* g1(a) { yield 0; for (arguments[0] of [28]); yield a; } var i1 = g1(0); i1.next(); out.push(i1.next().value);
function* g2(a) { yield 0; [arguments[0]] = [29]; yield a; } var i2 = g2(0); i2.next(); out.push(i2.next().value);
function* g3(a) { yield 0; for ([a] of [[30]]); yield arguments[0]; } var i3 = g3(0); i3.next(); out.push(i3.next().value);
out.join(" ")
"#;
    assert_eq!(value(src), "7 k 8 9 10 11 12 13 14 15 16 17 18 19 20 21 z 22 23,0 25,25 26 number27 28 29 30");
    let src = r#"async function h1(a) { await 0; for (arguments[0] of [31]); return a; }
async function h2(a) { await 0; ({ a } = { a: 32 }); return arguments[0]; }
async function* h3(a) { yield 0; for (arguments[0] in { w: 1 }); yield a; }
var i4 = h3(0);
Promise.all([h1(0), h2(0), i4.next().then(() => i4.next())]).then(v => { globalThis.out = [v[0], v[1], v[2].value].join(" "); });
"#;
    assert_eq!(out_after_jobs(src), "31 32 w");
}

/// More than 2^16 entries in one function's tables: inline caches (a
/// script's `var`s, its properties), constants (strings, numbers that
/// aren't int32s) and nested functions. Their operands were 2 bytes, and
/// an index past 65535 wrapped to another entry's: 18,000 `var`s
/// initialized from calls called a number literal (#176).
#[test]
fn a_function_indexes_more_than_65536_caches_constants_and_closures() {
    let n = 18000;
    let mut src = String::from("function f(n){return n}\n");
    for i in 0..n {
        src += &format!("var v{i} = f({i}); var g{i} = function(){{return {i}}};\n");
    }
    src += &format!("v{} + g{}();", n - 1, n - 1);
    assert_eq!(value(&src), "35998");

    let n = 70000;
    let last = n - 1;
    let each = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<String>();
    let cases = [
        // Globals, each its own cache.
        (each(&|i| format!("var v{i} = {i};\n")), format!("v{last} + v100"), last + 100),
        // Nested functions.
        (each(&|i| format!("a.push(function(){{return {i}}});\n")), format!("a[{last}]() + a[100]()"), last + 100),
        // String constants.
        (each(&|i| format!("a.push('{i}');\n")), format!("+a[{last}] + +a[100]"), last + 100),
        // An object literal's properties (caches that define them).
        (
            format!("var o = {{{}}};\n", each(&|i| format!("k{i}:{i},"))),
            format!("o.k{last} + o.k100 + Object.keys(o).length"),
            last + 100 + n,
        ),
        // Property stores, in a function.
        (
            format!(
                "function h(){{var o = {{}};\n{}return o;}}\nvar o = h();\n",
                each(&|i| format!("o.p{i} = {i};\n"))
            ),
            format!("o.p{last} + o.p100"),
            last + 100,
        ),
    ];
    for (body, expr, want) in cases {
        assert_eq!(value(&format!("var a = [];\n{body}{expr};")), want.to_string(), "{expr}");
    }
    // Numbers that aren't int32s: each its own constant.
    let src = format!("var s = 0;\n{}s;", each(&|i| format!("s += {i}.5;\n")));
    assert_eq!(value(&src), "2450000000");
}

/// The JIT reads the 4-byte operands as well (#176). Three functions each
/// fill one table past 2^16 with code that never runs, then run a loop
/// whose operations index it past 65535: property loads and stores, a
/// global's load and store and a call (caches); a number that isn't an
/// int32, strings, a regular expression literal's pattern and flags, an
/// object literal's getter name (constants); and a function expression
/// (nested functions). Each loop compiles, enters its compiled code, and
/// runs 3000 times, past the caches' first misses. (With the JIT reading
/// 2 bytes of them, all three come out wrong.)
#[test]
fn compiled_loops_read_indexes_past_65535() {
    let n = 66000;
    let fill = |f: &dyn Fn(usize) -> String| (0..n).map(f).collect::<String>();
    let src = format!(
        "var G = 3;
function id(v) {{ return v; }}
function hic(n) {{
  var o = {{}};
  if (n < 0) {{ {} }}
  var s = 0;
  for (var i = 0; i < n; i++) {{ o.r = i; s += o.r + G + id(1); G = G ^ 1; }}
  return s + ':' + o.r;
}}
function hconst(n) {{
  var x;
  if (n < 0) {{ {} }}
  var s = 0, t = '', c = 0;
  for (var i = 0; i < n; i++) {{
    s += 1.25;
    t = 'zq' + (i & 3);
    var re = /z+q/gy;
    c += re.source.length + re.flags.length;
    var a = {{ get kk() {{ return 2; }} }};
    c += a.kk;
  }}
  return s + ':' + t + ':' + c;
}}
function hfn(n) {{
  var x;
  if (n < 0) {{ {} }}
  var s = 0;
  for (var i = 0; i < n; i++) {{ var f = function(k) {{ return k * 2; }}; s += f(i); }}
  return s;
}}
[hic(3000), hconst(3000), hfn(3000)].join();",
        fill(&|i| format!("o.p{i};")),
        fill(&|i| format!("x = 's{i}';")),
        fill(&|_| "x = function(){};".to_string()),
    );
    // (SpiderMonkey's.)
    let want = "4509000:2999,3750:zq3:21000,8997000";
    let m = cloison::js::compile(&src).unwrap();
    for budget in [i64::MAX, 100] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        assert_eq!(display(vm.run(&m).unwrap()), want, "tier at {budget}");
        if budget == 100 {
            for f in ["hic", "hconst", "hfn"] {
                let st = vm.jit_stats(f).unwrap_or_else(|| panic!("{f} not compiled"));
                // Compiled code entered at the loop, and left at few of its
                // 3000 iterations (`hconst` at the regular expression's
                // `source` and `flags`, about 30 times).
                assert!(st.compiles >= 1 && st.osr_entries >= 1 && st.exits <= 100, "{f}: {st:?}");
            }
        }
    }
}

/// The `arguments` an arrow sees in a function with non-simple
/// parameters, and those of async functions, async generators and
/// methods with them, are unmapped: `callee` is the %ThrowTypeError%
/// accessor (#271). Also when the JIT inlines the function and makes
/// its `arguments` itself.
#[test]
fn arguments_callee_throws_in_more_non_simple_functions() {
    let src = r#"
var thrower = Object.getOwnPropertyDescriptor((function () { "use strict"; return arguments; })(), "callee").get;
function kind(a) {
    var d = Object.getOwnPropertyDescriptor(a, "callee");
    return d.get === thrower && d.set === thrower ? "thrower" : "get" in d ? "other" : "data";
}
var log = [];
function viaArrow(a = 1) { return (() => arguments)(); }
function inDefaultArrow(a, b = () => arguments) { return b(); }
function simpleArrow(a) { return (() => arguments)(); }
var o = { m(a = 1) { return arguments; }, n(a) { return arguments; } };
log.push(kind(viaArrow()), kind(inDefaultArrow()), kind(simpleArrow()), kind(o.m()), kind(o.n()));
function inner(a = 1) { return arguments; }
function outer(i) { return kind(inner(i)); }
var bad = 0;
for (var i = 0; i < 3000; i++) if (outer(i) !== "thrower") bad++;
log.push(bad);
async function af(a = 1) { return arguments; }
async function* ag(...r) { yield arguments; }
af().then(a => { log.push(kind(a)); return ag().next(); })
    .then(r => { log.push(kind(r.value)); globalThis.out = log.join(" "); });
"#;
    assert_eq!(out_after_jobs(src), "thrower thrower data thrower data 0 thrower thrower");
}

/// Folding looks at most `NUM_CONST_DEPTH` operators down: a long chain
/// of `+ 1` after a variable compiles in linear time and, past the front
/// end's depth, is the "too deeply nested" SyntaxError, as before folding
/// (it overflowed the stack while folding was tried at every operator).
/// A chain of literals longer than the bound still folds where it can.
#[test]
fn long_operator_chains_fold_within_a_bound() {
    // (On a big stack: a debug build's frames, larger, don't fit a test
    // thread's for chains this long.)
    let n = 400;
    assert_eq!(big_stack(|| value(&format!("var x = 1; x{}", "+1".repeat(n)))), (n + 1).to_string());
    assert_eq!(big_stack(|| value(&format!("1{}", "+1".repeat(n)))), (n + 1).to_string());
    assert_eq!(value(&format!("2 ** 3 * (1{})", " - 0.5".repeat(40))), "-152");
    let e = big_stack(|| compile_error(&format!("var x = 1; x{}", "+1".repeat(200_000))));
    assert!(e.contains("too deeply nested"), "{e}");
}

#[test]
fn typed_array_length_read_on_class_alone_until_one_differs() {
    // Compiled code reads any typed array's `length` from its header,
    // as an array's, while every one's is the builtin getter's
    // (`jit::typed_lengths_standard`); each way one can differ drops it.
    assert_eq!(
        value(
            r#"
function len(o) { return o.length; }
function warm(xs) { var r; for (var i = 0; i < 3000; i++) for (var j = 0; j < xs.length; j++) r = len(xs[j]); return r; }
var a = [1, 2], t = new Uint8Array(3), out = [warm([a, t, 'abcd'])];
Object.defineProperty(Int16Array.prototype, 'length', {value: 'i16'});
out.push(warm([a, new Int16Array(2)]));
out.join();
"#
        ),
        "4,i16"
    );
    assert_eq!(
        value(
            r#"
function len(o) { return o.length; }
function warm(xs) { var r; for (var i = 0; i < 3000; i++) for (var j = 0; j < xs.length; j++) r = len(xs[j]); return r; }
var a = [1, 2], t = new Uint8Array(3), out = [warm([a, t, 'abcd'])];
Object.defineProperty(Uint8Array.prototype, 'length', {value: 'u8', configurable: true});
out.push(warm([a, t]));
delete Uint8Array.prototype.length;
out.push(warm([a, t]));
out.join();
"#
        ),
        "4,u8,3"
    );
    assert_eq!(
        value(
            r#"
function len(o) { return o.length; }
function warm(xs) { var r; for (var i = 0; i < 3000; i++) for (var j = 0; j < xs.length; j++) r = len(xs[j]); return r; }
var a = [1, 2], t = new Uint8Array(3), out = [warm([a, t, 'abcd'])];
var proto = Object.getPrototypeOf(Int8Array.prototype);
Object.defineProperty(proto, 'length', {get: function () { return 'g'; }});
out.push(warm([a, t]));
out.join();
"#
        ),
        "4,g"
    );
    assert_eq!(
        value(
            r#"
function len(o) { return o.length; }
function warm(xs) { var r; for (var i = 0; i < 3000; i++) for (var j = 0; j < xs.length; j++) r = len(xs[j]); return r; }
var a = [1, 2], t = new Uint8Array(3), out = [warm([a, t, 'abcd'])];
function F() {}
F.prototype = Object.create(Float32Array.prototype, {length: {value: 'F'}});
out.push(warm([a, Reflect.construct(Float32Array, [1], F)]));
out.join();
"#
        ),
        "4,F"
    );
    assert_eq!(
        value(
            r#"
function len(o) { return o.length; }
function warm(xs) { var r; for (var i = 0; i < 3000; i++) for (var j = 0; j < xs.length; j++) r = len(xs[j]); return r; }
var a = [1, 2], t = new Uint8Array(3), out = [warm([a, t, 'abcd'])];
Object.setPrototypeOf(t, {length: 'p'});
out.push(warm([a, t]));
out.join();
"#
        ),
        "4,p"
    );
    assert_eq!(
        value(
            r#"
function len(o) { return o.length; }
function warm(xs) { var r; for (var i = 0; i < 3000; i++) for (var j = 0; j < xs.length; j++) r = len(xs[j]); return r; }
var a = [1, 2], t = new Uint8Array(3), out = [warm([a, t, 'abcd'])];
Object.defineProperty(t, 'length', {value: 'own'});
out.push(warm([a, t]));
out.join();
"#
        ),
        "4,own"
    );
}
