//! The JS front end: it parses every microbenchmark, compiles the ones the
//! instruction set covers to exactly the reference listings in
//! `bench/bytecode/`, and the compiled scripts compute what they should, in
//! the interpreter and through the JIT.

use std::path::{Path, PathBuf};

use toi::vm::{Vm, display};

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
    let module = toi::js::compile(src)?;
    let mut result = None;
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
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

fn compile_error(src: &str) -> String {
    toi::js::compile(src).expect_err(src)
}

#[test]
fn every_microbenchmark_parses() {
    for f in files("bench/micro", "js") {
        let src = std::fs::read_to_string(&f).unwrap();
        if let Err(e) = toi::js::parse(&src) {
            panic!("{}:{}", f.display(), e.display(&src));
        }
    }
}

#[test]
fn listings_are_what_the_front_end_emits() {
    for f in files("bench/bytecode", "asm") {
        let js = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("bench/micro")
            .join(f.with_extension("js").file_name().unwrap());
        let want = toi::asm::parse(&std::fs::read_to_string(&f).unwrap()).unwrap();
        let got = toi::js::compile(&std::fs::read_to_string(&js).unwrap())
            .unwrap_or_else(|e| panic!("{}:{e}", js.display()));
        let (want_text, got_text) = (toi::asm::disassemble(&want), toi::asm::disassemble(&got));
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
    "01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16",
    "17", "18", "19", "20", "21", "22", "23", "24", "25", "26", "27", "28", "29", "30", "31", "32",
    "33", "34", "35", "36", "37", "38", "39", "40", "41", "42", "43", "44", "45", "46", "50", "51",
    "52", "53", "54", "60", "61", "62", "63", "64", "65", "66", "67", "68", "69", "70", "71", "80",
    "81", "82", "83", "84", "85", "86", "87", "90", "91", "92", "93", "94",
];

#[test]
fn compiled_microbenchmarks_compute_their_expected_results() {
    let mut ran = Vec::new();
    for f in files("bench/micro", "js") {
        let src = std::fs::read_to_string(&f).unwrap();
        let name = f.file_name().unwrap().to_str().unwrap();
        let expect = src
            .lines()
            .find_map(|l| l.strip_prefix("// expect: "))
            .unwrap()
            .trim();
        let Ok(module) = toi::js::compile(&src) else {
            continue;
        };
        let mut vm = Vm::new();
        // Whatever runs to completion must be right; what doesn't must
        // fail on something the VM doesn't have yet.
        match vm.run(&module) {
            Ok(v) => {
                assert_eq!(display(v), expect, "{name}");
                ran.push(name[..2].to_string());
            }
            Err(e) => {
                let e = e.to_string();
                assert!(
                    e.contains("is not a function") || e.contains("is not defined"),
                    "{name}: {e}"
                );
            }
        }
    }
    assert_eq!(ran, RUNNING);
    // The ones that run do so through the JIT too.
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if RUNNING.contains(&&name[..2]) {
            let src = std::fs::read_to_string(&f).unwrap();
            let expect = src
                .lines()
                .find_map(|l| l.strip_prefix("// expect: "))
                .unwrap();
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
        if !RUNNING.contains(&&name[..2]) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        let expect = src
            .lines()
            .find_map(|l| l.strip_prefix("// expect: "))
            .unwrap()
            .trim();
        let module = toi::js::compile(&src).unwrap();
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
        if !RUNNING.contains(&&name[..2]) {
            continue;
        }
        let src = std::fs::read_to_string(&f).unwrap();
        let expect = src
            .lines()
            .find_map(|l| l.strip_prefix("// expect: "))
            .unwrap()
            .trim();
        let module = toi::js::compile(&src).unwrap();
        for budget in [1, 1000] {
            let mut vm = Vm::new();
            vm.set_warm_tier(true);
            vm.set_tier_back_edges(budget);
            let v = vm.run(&module).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert_eq!(display(v), expect, "{name} (warm, tier at {budget})");
        }
    }
}

#[test]
fn statements() {
    // `while`, `do … while`, `break` and `continue`, labeled ones too.
    assert_eq!(
        value(
            "function f() {
               var n = 0, i = 0;
               while (i < 100) { i++; if (i % 3 == 0) continue; n += i; }
               do { n++; } while (n < 3000);
               outer: for (var a = 0; a < 10; a++)
                 for (var b = 0; b < 10; b++) { if (b == 5) continue outer; if (a == 7) break outer; n += 1000; }
               return n;
             }
             f();"
        ),
        "38368"
    );
    // A loop whose first test fails; one with a computed bound; an endless
    // one left by `return`.
    assert_eq!(
        value(
            "function f(k) {
               var n = 0;
               for (var i = 5; i < 3; i++) n += 100;
               for (var a = 9, b = 0; a < 3; a++) n += 100;
               for (var j = 0; j < k; j++) n++;
               while (true) { n += 2; if (n == 11) return n; }
             }
             f(5);"
        ),
        "11"
    );
    // A labeled block; `if`/`else` chains; an early return inside a loop.
    assert_eq!(
        value(
            "function f(x) {
               b: { if (x == 1) break b; return 10; }
               for (var i = 0; i < 10; i++) if (i == x + 2) return i; else if (i == 9) return -1;
             }
             function g() { return f(1) + f(2); }
             g();"
        ),
        "13"
    );
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
    assert_eq!(
        value("function f() { var x = 1; return x + (x = 10); } f();"),
        "11"
    );
    assert_eq!(
        value("function f() { var x = 1; x += (x = 10); return x; } f();"),
        "11"
    );
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

#[test]
fn the_script_returns_its_last_expression_statement() {
    assert_eq!(value("var x; x == undefined;"), "true");
    assert_eq!(
        value("var s = 0; for (var i = 0; i < 5; i++) s += i; s;"),
        "10"
    );
    assert_eq!(value("var s = 1;"), "undefined");
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

#[test]
fn rejects_what_it_cannot_compile() {
    let unsupported = [
        ("var g = function h() { return h; };", "own name"),
        ("function f(i) { return i++; }", "postfix"),
        ("var x = -y;", "Neg"),
        ("try { 1; } finally { 2; }", "finally"),
        ("var o = {1: 2};", "the key `1`"),
        ("function f(a, b) { return a % b; }", "`%`"),
    ];
    for (src, what) in unsupported {
        let e = compile_error(src);
        assert!(e.contains("unsupported") && e.contains(what), "{src}: {e}");
    }
    assert!(compile_error("var a = 1;\nvar b = ;").starts_with("2:9:"));
    assert!(compile_error("while (1) { continue x; }").contains("no target"));
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
        (
            "rel([1, 2, 3, 5 / 2, nan, 0 * -1, 0], [2, 0, 5 / 2, nan])",
            "3400",
        ),
        ("loops(1000)", "417668"),
        ("nulls()", "433260"),
    ] {
        assert_eq!(value(&format!("{GROUP2}\n{call};")), want, "{call}");
    }
}

/// Two calls inlined into one loop share their frames' registers: `at`'s
/// int32 parameter and the array `size` loads into the same register. The
/// `length` load after it was compiled as an exit to the interpreter
/// (a bail), which reads that register: it must not have been kept as an
/// unboxed int32 (which truncated the array).
#[test]
fn inlined_frames_sharing_registers_survive_an_exit() {
    let src = "
        function Coll() { this.elms = []; }
        Coll.prototype.add = function (e) { this.elms.push(e); };
        Coll.prototype.at = function (i) { return this.elms[i]; };
        Coll.prototype.size = function () { return this.elms.length; };
        function collect(n) { var out = new Coll(); for (var i = 0; i < n; i++) out.add(i); return out; }
        function main() {
          var sum = 0;
          for (var r = 0; r < 3000; r++) {
            var c = collect(50);
            for (var i = 0; i < c.size(); i++) sum += c.at(i);
          }
          return sum;
        }
        main();";
    assert_eq!(value(src), "3675000");
}

/// Inlined callees read parameters they never write from the caller's
/// argument registers; an exit inside them (here int32 overflows in `add`,
/// nested in `both`, and a shape miss in `scale`) copies those into the
/// callee's frame for the interpreter. The expected value is Node's.
#[test]
fn parameters_read_from_arguments_survive_exits() {
    let src = "
        function P(x) { this.x = x; }
        P.prototype.add = function (a, b) { return this.x + a + b; };
        P.prototype.both = function (a, b) { return this.add(a, b) * 2 + a; };
        function Q(x) { this.x = x; this.y = 1; }
        Q.prototype.add = function (a, b) { return this.x - a + b; };
        Q.prototype.both = function (a, b) { return this.add(b, a) + b; };
        function scale(o, k) { return o.x * k + k; }
        function main() {
          var t = 0, i, ps = [new P(1), new Q(2)], p = new P(3);
          for (i = 0; i < 3000; i++) {
            var big = i < 2500 ? i : 2147483644;
            t = (t + p.both(big, i & 7)) % 1000003;
            t = (t + ps[i & 1].both(i, big)) % 1000003;
            var o = i < 2900 ? p : ps[1];
            t = (t + scale(o, i & 3)) % 1000003;
          }
          return t;
        }
        main();";
    assert_eq!(value(src), "955175");
}

/// Loads of a global function and of `C.parent.prototype.get.call` compile
/// to constants behind checks of their slots (or none, for a prototype's):
/// the global reassigned, `C.parent` switched, the prototype's method
/// replaced and `call` shadowed by an own property, each midway, are seen.
/// The expected value is Node's.
#[test]
fn constant_loads_see_their_slots_change() {
    let src = "
        function A() { this.v = 1; }
        A.prototype.get = function (k) { return this.v + k; };
        function B() { this.v = 2; }
        B.prototype.get = function (k) { return this.v * k; };
        function C() {}
        C.parent = A;
        var helper = function (x) { return x + 1; };
        function main() {
          var t = 0, i, o = new A();
          for (i = 0; i < 4000; i++) {
            if (i === 1000) helper = function (x) { return x + 2; };
            if (i === 2000) C.parent = B;
            if (i === 2500) A.prototype.get = function (k) { return this.v - k; };
            if (i === 3000) B.prototype.get.call = function (that, k) { return 1000; };
            t += helper(i & 7);
            t += C.parent.prototype.get.call(o, i & 3);
          }
          return t;
        }
        main();";
    assert_eq!(value(src), "1027500");
}

/// Checks of constant loads moved before their loops (`jit::hoist_guards`):
/// a `for` entered at its header, a `while` entered by a jump to its
/// condition inside another loop, and a `while` alone. Between runs of the
/// loops the checked slots change (a property, the global, a field turning
/// double): the checks before the loops fail and the loads are compiled
/// again. The expected value is Node's.
#[test]
fn checks_moved_before_loops_see_their_slots_change() {
    let src = "
        var Dir = new Object();
        Dir.NONE = 0; Dir.FORWARD = 1; Dir.BACKWARD = -1;
        var K = { scale: 3 };
        function count(n) {
          var sum = 0;
          for (var i = 0; i < n; i++) {
            var d = (i % 3) - 1;
            if (d == Dir.FORWARD) sum += 2;
            else if (d == Dir.BACKWARD) sum += 3;
            else if (d == Dir.NONE) sum += 5;
          }
          return sum;
        }
        function nested(n) {
          var t = 0;
          for (var r = 0; r < n; r++) {
            var j = 0;
            while (j < 50) { t += K.scale * j; j++; }
            t = t % 1000003;
          }
          return t;
        }
        function viaWhile(n) {
          var t = 0, i = 0;
          while (i < n) { t += Dir.FORWARD - Dir.BACKWARD; i++; }
          return t;
        }
        function main() {
          var out = \"\";
          for (var round = 0; round < 6; round++) {
            out = out + count(3000) + \",\" + nested(200) + \",\" + viaWhile(3000) + \";\";
            if (round == 1) Dir.FORWARD = 7;
            if (round == 2) { Dir = { NONE: 0, FORWARD: 1, BACKWARD: 2 }; }
            if (round == 3) K.scale = 5;
            if (round == 4) { K = { scale: 2.5 }; Dir.BACKWARD = -1; }
          }
          return out;
        }
        main();";
    assert_eq!(
        value(src),
        "10000,735000,6000;10000,735000,6000;8000,735000,24000;7000,735000,-3000;\
         7000,224997,-3000;10000,612500,6000;"
    );
}

/// A recursive function whose parameter is an object of a shape or one
/// extending it (`extra` added to some nodes, or another constructor's
/// shape): its compiled entry takes `Ext` of the shape, so its calls to
/// itself with loaded nodes are direct. The `extra` store, not compiled
/// (a bail) in the copy of `build` inlined into itself, once made the
/// inlined frame's registers look live at the function's entry, which
/// then read leftovers from the stack as objects (a crash). The expected
/// value is Node's.
#[test]
fn recursive_calls_with_extending_shapes_compute_the_same() {
    let src = "
        function Node(i) { this.value = i; this.kids = []; }
        function Other(i) { this.kids = []; this.value = i * 2; }
        function build(depth, i) {
          var n = depth % 3 == 0 ? new Other(i) : new Node(i);
          if (i % 5 == 0) n.extra = i;
          if (depth > 0) { n.kids.push(build(depth - 1, 2 * i)); n.kids.push(build(depth - 1, 2 * i + 1)); }
          return n;
        }
        function sum(n) {
          var s = n.value + (n.extra === undefined ? 0 : 1);
          for (var i = 0; i < n.kids.length; i++) s += sum(n.kids[i]);
          return s;
        }
        function main() {
          var t = 0;
          for (var r = 0; r < 30; r++) t = (t + sum(build(8, r + 1))) % 1000003;
          return t;
        }
        main();";
    assert_eq!(value(src), "890162");
}

/// Strings: concatenation with numbers of every format, `length`, `==` and
/// `===` between distinct strings and with numbers, `<` on strings,
/// ToBoolean, numeric strings in arithmetic, arrays as strings, strings in
/// fields. The expected values are Node's.
const STRINGS: &str = r#"
function concat() {
  var s = "", i, xs = [0, -1, 7, 2147483647, 0.5, -2.25, 1e21, 1.5e-7, 123.456, 1 / 3, 0 / 0, 1 / 0, -1 / 0, 0 * -1, 100, 1e300 * 10, 4294967296, 0.000001, 123456789012345680000];
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
            "|0|-1|7|2147483647|0.5|-2.25|1e+21|1.5e-7|123.456|0.3333333333333333|NaN|Infinity|-Infinity|0|100|1e+301|4294967296|0.000001|123456789012345680000",
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
    assert_eq!(
        value(&format!(
            "{POLY}\nrun(2, 300) + run(3, 300) + run(4, 300) + run(5, 300);"
        )),
        "9152700"
    );
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
        (
            "run(2, 300) + run(4, 300) + run(5, 300) + later()",
            "47476350",
        ),
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
    for (call, want) in [
        ("stores()", "2262261.5"),
        ("stack()", "2847651"),
        ("ctors()", "913365704"),
        ("mixed()", "19005"),
    ] {
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
    assert_eq!(
        value(&format!("{MIXED_ARRAYS}\nmain(3) + main(5);")),
        "426748383"
    );
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
