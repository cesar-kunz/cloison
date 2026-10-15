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
        vm.heap.verify();
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
    "81", "82", "83", "84", "85", "86", "87", "90", "91", "92", "93", "94", "100", "101", "102",
    "103", "104", "105", "106", "107", "108", "109", "110", "111", "112", "113", "114", "115",
    "116", "117", "118", "119", "120", "121", "122", "123", "124", "125", "126", "127", "128",
    "129", "130", "131", "132", "133", "134", "135",
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
                ran.push(number(name).to_string());
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
    // (Files sort as text: `100-` before `11-`.)
    ran.sort_by_key(|n| n.parse::<u32>().unwrap());
    assert_eq!(ran, RUNNING);
    // The ones that run do so through the JIT too.
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if RUNNING.contains(&&number(name)) {
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
        if !RUNNING.contains(&&number(name)) {
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
        if !RUNNING.contains(&&number(name)) {
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
    let unsupported: [(&str, &str); 0] = [];
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

/// Code compiled for an outer loop, entered at the inner loop's header
/// (tiering up there): the stub that zeroes dead registers once per
/// collection must go on to that header, not to the region's first
/// instruction (`j = 0`), which ran the inner loop's iterations again.
#[test]
fn entering_a_nested_loop_after_zeroing_dead_registers() {
    assert_eq!(
        value(
            "var s = 0;
             for (var i = 0; i < 3; i++) for (var j = 0; j < 3; j++) { s = s * 3 + j + i; }
             s;"
        ),
        "4162"
    );
}

/// `%` with a register divisor (`Op::Mod`): the int32 stencil, its exits
/// (-0, a zero or negative divisor) and the double form after them (which
/// exits for a zero divisor), and the generic one; against Node's values.
/// Fails if `mod_ii` keeps a negative dividend's zero remainder.
#[test]
fn modulo_agrees_with_node() {
    let src = r#"
// `r % acc` with a register divisor: int32s first, then the cases that
// leave the int32 fast path (a negative dividend's zero remainder is -0,
// zero and negative divisors, INT32_MIN % -1), then doubles and strings.
function show(x) { return x === 0 && 1 / x < 0 ? "-0" : String(x); }
function ints(n, d) {
  var s = 0;
  for (var i = 0; i < n; i++) s += i % d + (i * 7) % (d + 3);
  return s;
}
function mixed(as, ds) {
  var out = [];
  for (var i = 0; i < as.length; i++)
    for (var j = 0; j < ds.length; j++) out.push(show(as[i] % ds[j]));
  return out.join(",");
}
function phases() {
  var s = "", t = 0;
  for (var k = 0; k < 3; k++) {
    for (var i = 1; i < 200; i++) t += (i * 13) % (i % 17 + 1);
    // After the loop compiled for int32s: exits, then the double form.
    for (var i = -50; i < 50; i++) {
      var d = i % 7 - 3;
      var r = i % (d === 0 ? 5 : d);
      s += show(r) + ";";
    }
  }
  return t + "|" + s;
}
// A positive divisor in a register, the dividend running down through
// negative multiples of it: -0 once the int32 form is compiled.
function negzero(d) {
  var s = "";
  for (var i = 60; i > -60; i--) s += show(i % d) + ";";
  return s;
}
// Zero divisors on the int32 path: `mod_ii` exits at the first, the
// double form at the next ones (NaN).
function zeros() {
  var s = "";
  for (var i = 1; i < 40; i++) s += show(i % (i % 4)) + ";";
  return s;
}
ints(1000, 9) + "|" + mixed([7, -7, 0, -0, 6, -6, 2147483647, -2147483648, 5.5, -5.5, 1e300, "8", NaN, Infinity],
  [3, -3, 1, -1, 0, 2.5, -2.5, Infinity, NaN, "4"]) + "|" + phases() + "|" + negzero(7) + "|" + zeros();
"#;
    assert_eq!(
        value(src),
        "9492|1,1,0,0,NaN,2,2,7,NaN,3,-1,-1,-0,-0,NaN,-2,-2,-7,NaN,-3,0,0,0,0,NaN,0,0,0,NaN,0,-0,-0,-0,-0,NaN,-0,-0,-0,NaN,-0,0,0,0,0,NaN,1,1,6,NaN,2,-0,-0,-0,-0,NaN,-1,-1,-6,NaN,-2,1,1,0,0,NaN,2,2,2147483647,NaN,3,-2,-2,-0,-0,NaN,-0.5,-0.5,-2147483648,NaN,-0,2.5,2.5,0.5,0.5,NaN,0.5,0.5,5.5,NaN,1.5,-2.5,-2.5,-0.5,-0.5,NaN,-0.5,-0.5,-5.5,NaN,-1.5,0,0,0,0,NaN,0,0,1e+300,NaN,0,2,2,0,0,NaN,0.5,0.5,8,NaN,0,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN|2001|-2;-1;-3;-7;-4;-3;-4;-3;-0;-5;-0;-4;-2;-2;-0;-2;-7;-1;-4;-1;-0;-1;-1;-0;-2;-4;-0;-3;-2;-0;-2;-3;-4;-5;-1;-3;-2;-4;-4;-4;-4;-4;-0;-1;-6;-5;-4;-3;-2;-1;0;1;0;3;0;1;0;1;0;0;0;0;0;1;2;1;0;2;0;1;2;0;0;0;4;0;0;0;1;1;0;1;0;1;1;2;0;0;3;0;0;2;0;1;0;0;0;1;0;1;-2;-1;-3;-7;-4;-3;-4;-3;-0;-5;-0;-4;-2;-2;-0;-2;-7;-1;-4;-1;-0;-1;-1;-0;-2;-4;-0;-3;-2;-0;-2;-3;-4;-5;-1;-3;-2;-4;-4;-4;-4;-4;-0;-1;-6;-5;-4;-3;-2;-1;0;1;0;3;0;1;0;1;0;0;0;0;0;1;2;1;0;2;0;1;2;0;0;0;4;0;0;0;1;1;0;1;0;1;1;2;0;0;3;0;0;2;0;1;0;0;0;1;0;1;-2;-1;-3;-7;-4;-3;-4;-3;-0;-5;-0;-4;-2;-2;-0;-2;-7;-1;-4;-1;-0;-1;-1;-0;-2;-4;-0;-3;-2;-0;-2;-3;-4;-5;-1;-3;-2;-4;-4;-4;-4;-4;-0;-1;-6;-5;-4;-3;-2;-1;0;1;0;3;0;1;0;1;0;0;0;0;0;1;2;1;0;2;0;1;2;0;0;0;4;0;0;0;1;1;0;1;0;1;1;2;0;0;3;0;0;2;0;1;0;0;0;1;0;1;|4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;|0;0;0;NaN;0;0;1;NaN;0;0;2;NaN;0;0;0;NaN;0;0;1;NaN;0;0;2;NaN;0;0;0;NaN;0;0;1;NaN;0;0;2;NaN;0;0;0;"
    );
}

/// An array of more than 1,024 values is marked a slice at a time, the
/// rest left on the mark stack (`Marking::drain`): objects reachable only
/// from its later slices, stored there young, must survive collections.
/// Fails under `TOI_GC_STW=1 TOI_GC_FULL=1 TOI_GC_VERIFY=2048` if the rest
/// of the array is dropped after the first slice.
#[test]
fn large_arrays_marked_in_slices_keep_their_later_values() {
    assert_eq!(
        value(
            r#"
function Box(v) { this.v = v; this.next = null; }
function run() {
  var big = new Array(5000), junk = new Array(256);
  for (var i = 0; i < 5000; i++) big[i] = new Box(i);
  for (var i = 0; i < 256; i++) junk[i] = new Box(0);
  var sum = 0;
  for (var round = 0; round < 60; round++) {
    // Garbage that escapes, so collections (and marking) run.
    for (var g = 0; g < 4000; g++) {
      junk[g & 255] = new Box(g);
      sum = (sum + junk[(g * 7) & 255].v) % 1000003;
    }
    // New young objects stored into the array's later slices.
    for (var i = 3000; i < 5000; i += 7) big[i] = new Box(i + round);
    for (var i = 0; i < 5000; i += 13) sum = (sum + big[i].v) % 1000003;
  }
  return sum;
}
run();
"#
        ),
        "626460"
    );
}

/// A pause traces on several threads past 16,384 cells
/// (`Marking::drain_parallel`), the threads handing each other parts of
/// their stacks: a tree and an array of 40,000 cells must survive. Fails
/// under `TOI_GC_STW=1 TOI_GC_FULL=1 TOI_GC_VERIFY=2048` if a handed-over
/// part is lost.
#[test]
fn heaps_traced_on_several_threads_keep_every_cell() {
    assert_eq!(
        value(
            r#"
function Node(v, l, r) { this.v = v; this.l = l; this.r = r; }
function tree(d, v) { return d === 0 ? new Node(v, null, null) : new Node(v, tree(d - 1, 2 * v), tree(d - 1, 2 * v + 1)); }
function sum(t) { return t === null ? 0 : (t.v + sum(t.l) + sum(t.r)) % 1000003; }
function run() {
  var t = tree(14, 1), list = new Array(8000), junk = new Array(64), s = 0;
  for (var i = 0; i < 8000; i++) list[i] = new Node(i, null, null);
  for (var i = 0; i < 64; i++) junk[i] = list[0];
  for (var round = 0; round < 30; round++) {
    for (var g = 0; g < 3000; g++) junk[g & 63] = new Node(g, null, null);
    s = (s + sum(t)) % 1000003;
    for (var i = 0; i < 8000; i += 97) s = (s + list[i].v) % 1000003;
  }
  return s;
}
run();
"#
        ),
        "490225"
    );
}

/// A function big enough for only four compilations (`max_compiles`)
/// whose loop overflows int32 at five instructions, one after another,
/// each exit widening one: past the fourth, the last version is kept and
/// entered (the values it was compiled for still fit at the loop header),
/// exiting again wherever it overflows, which it was compiled before
/// widening. Before, the function ran in the interpreter from there on
/// (zlib's `a1`, for most of its run).
#[test]
fn a_function_out_of_compilations_keeps_its_last_version() {
    let pad: String = (0..900)
        .map(|k| format!("    s = (s + (i ^ {k}) * {}) | 0;\n", k % 7 + 1))
        .collect();
    let src = format!(
        "function big(n, neg) {{
  var s = 0;
  for (var i = 0; i < n; i++) {{
{pad}    var u = (i % 1000 === 100 ? neg : i) >>> 0;
    var z = -(i % 1000 === 200 ? 0 : 1);
    var m = (i % 1000 === 300 ? 65536 : 1) * 65536;
    var a = (i % 1000 === 400 ? 2147483647 : 1) + i;
    var w = (i % 1000 === 500 ? neg - i : i) >>> 0;
    s = (s + u % 1000 + (1 / z > 0 ? 1 : 2) + m % 999 + a % 997 + w % 991) % 1000003;
  }}
  return s;
}}
big(8000, -5);"
    );
    assert_eq!(value(&src), "662330");
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
    assert_eq!(value(&format!("{CHANGED_CONSTANTS}
r;")), "9420847");
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
    assert_eq!(value(&format!("{CHANGED_CONSTANTS_2}
r;")), "13721319");
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
    assert_eq!(value(&format!("{SHADOWED_METHODS}
r;")), "2110380");
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
    assert_eq!(value(&format!("{HOISTED_LOADS}
r;")), "1447227");
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
    assert_eq!(value(&format!("{HOISTED_FROM_NESTED_LOOPS}
r;")), "542668");
}

/// `this` in sloppy mode (toi has no strict mode): the global object in a
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
    assert_eq!(
        value(&format!("{ACCESSORS}\naccessors();")),
        "4922724:_key,f,_v,v,nested:3:truefalse"
    );
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

/// Upvalues that held int32s when compiled are loaded as int32s, checked
/// (exiting when one changed: then compiled as any value); a double
/// compared with `>`, `<=`, `>=` compares without the runtime, NaN
/// included. The expected value is Node's.
#[test]
fn upvalues_speculated_int32_change_type() {
    let src = r#"
function Grid(n) {
  var width = n, scale = 2, s = 0;
  function sum(a) { var t = 0; for (var i = 0; i < width; i++) t += a[i] * scale + width; return t; }
  function less(x) { var c = 0; for (var i = 0; i < width; i++) if (x > i + 0.5) c++; return c; }
  return {
    sum: sum, less: less,
    setScale: function (v) { scale = v; }, setWidth: function (v) { width = v; }
  };
}
var g = Grid(50), a = [];
for (var i = 0; i < 60; i++) a.push(i * 1.5);
var r = [];
for (var k = 0; k < 300; k++) s0 = g.sum(a);
r.push(g.sum(a), g.less(20.25), g.less(NaN));
g.setScale(0.5);
r.push(g.sum(a));
g.setWidth(10.5);
r.push(g.sum(a), g.less(7.75));
g.setScale("3");
r.push(g.sum(a));
g.setWidth(60);
for (var k = 0; k < 300; k++) s0 = g.sum(a);
r.push(g.sum(a), g.less(1e9));
r.join(",");
"#;
    assert_eq!(value(src), "6175,20,0,3418.75,156.75,8,363,11565,60");
}

/// Temporaries split off their registers (`jit::split_temps`): exits
/// between one's store and its reads (a string, `null` among the
/// numbers) leave the interpreter its register as the bytecode had it.
/// The expected value is Node's.
#[test]
fn exits_within_split_temporaries() {
    let src = r#"
function mix(a, b, c, n) {
  var s = 0, t = "";
  for (var i = 0; i < n; i++) {
    s = (a[i] * 1.5 + b[i] * 2.25) * (c[i] + 0.5) + s * 0.25;
    t = a[i] - (b[i] + c[i] * 3);
  }
  return s + ":" + t;
}
function fill(n, f) { var r = []; for (var i = 0; i < n; i++) r.push(f(i)); return r; }
var a = fill(200, function (i) { return i * 0.5; });
var b = fill(200, function (i) { return i % 7; });
var c = fill(200, function (i) { return (i % 5) * 1.25; });
var out = [];
for (var k = 0; k < 50; k++) out.push(mix(a, b, c, 200));
b[150] = "x"; c[160] = null; a[170] = "12";
out.push(mix(a, b, c, 200));
b[150] = 3;
for (var k = 0; k < 50; k++) out.push(mix(a, b, c, 200));
out.slice(48, 53).join(",");
"#;
    assert_eq!(
        value(src),
        "1054.0532493329365:81.5,1054.0532493329365:81.5,NaN:81.5,1054.0532493329365:81.5,1054.0532493329365:81.5"
    );
}

/// ToPrimitive calls bytecode `valueOf` and `toString` (`interp::
/// to_primitive_hint`): in the interpreter's slow paths, in natives
/// (`Math.max`), and for compiled code by leaving it for the interpreter
/// (`jit::RT_REDO`), once each; what one throws propagates; neither
/// giving a primitive is a TypeError. The expected value is Node's.
#[test]
fn value_of_and_to_string_convert_objects() {
    let src = r#"
var calls = 0;
function V(n) { this.n = n; }
V.prototype.valueOf = function () { calls++; return this.n; };
V.prototype.toString = function () { return "V" + this.n; };
var a = new V(4), b = new V(10);
var r = [];
r.push(a * 3, a + 1, a - b, a / 2, a % 3, -a, a | 1, a << 2, a > 3, a < b, a == 4, a + "x", "x" + a, String(a), a + b);
var o2 = { toString: function () { return "7"; } };
r.push(o2 * 2, o2 + 1, Math.max(a, o2), [1, 2, 3][o2 > 5 ? 1 : 0]);
var thrower = { valueOf: function () { throw new Error("boom"); } };
try { r.push(thrower + 1); } catch (e) { r.push("caught " + e.message); }
try { r.push(Math.floor(thrower)); } catch (e) { r.push("caught2 " + e.message); }
var bad = { valueOf: function () { return {}; }, toString: function () { return {}; } };
try { r.push(bad * 1); } catch (e) { r.push(e instanceof TypeError); }
// In a hot loop: compiled code meeting objects.
function sum(xs) { var s = 0; for (var i = 0; i < xs.length; i++) s = s + xs[i] * 2 - 1; return s; }
var xs = []; for (var i = 0; i < 2000; i++) xs.push(i % 7);
var t = 0; for (var k = 0; k < 20; k++) t += sum(xs);
xs[1500] = new V(100); t += sum(xs);
function lt(xs) { var c = 0; for (var i = 0; i < xs.length; i++) if (xs[i] < 3.5) c++; return c; }
for (var k = 0; k < 20; k++) t += lt(xs);
xs[10] = new V(1); t += lt(xs);
r.push(t, calls);
r.join(",");
"#;
    assert_eq!(
        value(src),
        "12,5,-6,2,1,-4,5,16,true,true,true,4x,x4,V4,14,14,71,7,2,caught boom,caught2 boom,true,233989,41"
    );
}

/// Functions with more variables than registers: the rest in a scope
/// object, also read by a nested function. The expected value is Node's.
#[test]
fn many_variables_agree_with_node() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/octane/many-variables.js.txt")).unwrap();
    assert_eq!(value(&src), "2001571,4543976");
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
    assert_eq!(value(&format!("{UPVALUES}\nnegation();")), "-397800000;1000;-Infinity,2147483648,-3,-5,-10|-2000500;166.66666666667243;-Infinity,2147483648,-3,-5,-10|1996000;-333.33333333334485;Infinity,2147483648,-3,-5,-10");
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
    assert_eq!(value(&format!("{UNTYPED_SHIFTS}\nuntypedShifts();")), "10899,-1251851505,-1251851529,116577075,116576975,116581250,116576750,116576850,499.5:499.75");
}

#[test]
fn regexp_last_index_and_exec_arrays() {
    // `lastIndex` read and stored in its slot, `exec` arrays given their
    // shape by a cache, literals found by search: as Node does (without
    // ES2018's `groups`).
    let src = r#"var out = [], re = /a(b)?/g, s = "xab ab a";
re.lastIndex = "2"; var m = re.exec(s); out.push(m.index, m[0], m[1], re.lastIndex);
re.lastIndex = 3.7; m = re.exec(s); out.push(m.index, m[1], re.lastIndex);
re.lastIndex = -4; m = re.exec(s); out.push(m.index, re.lastIndex);
re.lastIndex = 99; out.push(re.exec(s), re.lastIndex);
m = re.exec(s); m = re.exec(s); m = re.exec(s); out.push(m.index, typeof m[1], re.lastIndex, re.test(s), re.lastIndex, re.test(s), re.lastIndex);
out.push(Object.keys(m).join("|"), m.input === s, m.length);
m.index = 7; m.extra = 1; out.push(Object.keys(m).join("|"));
var n = /(\d)(\d)?/.exec("a1b"); out.push(Object.keys(n).join("|"), n.index, n.input, n.length);
var lit = /ab/; out.push(lit.exec("zzab").index, lit.test("a b"), "xxabab".replace(/ab/, "-"), "a,b,,c".split(/,/).length);
out.join(",");"#;
    assert_eq!(value(src), "4,ab,b,6,4,b,6,1,3,,0,7,undefined,8,false,0,true,3,0|1|index|input,true,2,0|1|index|input|extra,0|1|2|index|input,1,a1b,3,2,false,xx-ab,4");
}

#[test]
fn global_replace_and_index_of_agree_with_node() {
    // A global `replace` collects every match's groups, unmatched ones
    // included, before it builds the result; a replacement function may
    // run the same pattern again. `indexOf` searches eight units at a time.
    let src = r#"var out = [], s = "k1=v1; k22=; k3=v33";
out.push(s.replace(/(\w+)=(\w*)/g, "$2:$1"));
out.push(s.replace(/(\w+)=(v(\d+))?/g, function (m, k, v, d, at) { return "[" + k + "|" + v + "|" + d + "|" + at + "]"; }));
var re = /x(y)?/g;
out.push("xyzxxy".replace(re, function (m, y) { return "xyxy".replace(re, "-$1-") + typeof y; }));
out.push("aaa".replace(/a*?/g, "<$&>"), "abc".replace(/q/g, "-"), "a.b.c".replace(".", "$$"), " t ".replace(/^\s*|\s*$/g, ""));
out.push("long haystack with needle inside, needle twice".indexOf("needle"), "abcabcabd".indexOf("abd", 2), "ab".indexOf("abc"), "aaaaaaaaab".lastIndexOf("b"));
out.join(",");"#;
    assert_eq!(
        value(src),
        "v1:k1; :k22; v33:k3,[k1|v1|1|0]; [k22|undefined|undefined|7]; [k3|v33|33|13],-y--y-stringz-y--y-undefined-y--y-string,<>a<>a<>a<>,abc,a$b.c,t,19,6,-1,9"
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
    let m = toi::js::compile(src).unwrap();
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
    let m = toi::js::compile(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(20);
    assert_eq!(display(vm.run(&m).unwrap()), "1515000");
    let st = vm.jit_stats("f").expect("f compiled");
    // (1 here; 20 when the exit at the call left it uncompiled.)
    assert!(st.exits <= 5, "{st:?}");
}

/// Runs `src` tiering up after 1000 back edges (the default, whatever
/// `TOI_TIER` says); returns its value and `function`'s stencils.
fn compiled(src: &str, function: &str) -> (String, Vec<String>) {
    let m = toi::js::compile(src).unwrap();
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

/// `slice`, `splice`, `shift`, `unshift`, `concat` and `reverse` move
/// elements in place (holes as they are), joining the elements' type once
/// per value.
#[test]
fn array_builtins_move_elements_in_place() {
    let src = r#"
var r = [];
var a = [1, 2, 3, 4, 5, 6, 7];
r.push(a.slice(2).join(), a.slice(-3, -1).join(), a.slice(5, 2).length);
var b = a.slice();
r.push(b.splice(1, 2).join(), b.join(), b.splice(2, 0, "x", "y").length, b.join());
r.push(b.splice(-2).join(), b.splice(0, 1, { k: 1 })[0], typeof b[0], b.length);
var c = [1.5, 2, 3];
r.push(c.shift(), c.unshift(9, 8), c.join(), [].shift());
r.push([1, 2].concat([3, [4]], 5).length, [3, 2, 1].reverse().join());
var h = [1, 2, 3]; delete h[1];
var g = [6, 7]; delete g[0];
r.push(1 in h.slice(), 3 in h.concat(g), h.reverse()[0], 1 in h);
var q = [], s = 0;
for (var i = 0; i < 300; i++) { q.unshift(i, i * 0.5); q.splice(q.length >> 1, 1); s += q[0]; }
while (q.length) s += q.shift();
r.push(s);
r.join(" ");"#;
    assert_eq!(
        value(src),
        "3,4,5,6,7 5,6 0 2,3 1,4,5,6,7 0 1,4,x,y,5,6,7 6,7 1 object 5 1.5 4 9,8,2,3  5 1,2,3 false false 3 false 86887.5"
    );
}

/// Strings of other lengths are unequal without flattening them.
#[test]
fn string_equality_by_length_first() {
    let src = r#"
var r = [];
var a = "ab"; a += "c";
var b = "a"; b += "bc";
var keys = ["abc", "abd", "ab", "abcd"];
for (var i = 0; i < keys.length; i++) r.push(a === keys[i], a == keys[i], keys[i] !== b);
r.push(a === b, a == b, "1" == 1, a == 3, "" == 0, null == "");
r.push(String.fromCharCode(65), String.fromCharCode(66, 67), String.fromCharCode(65.7), String.fromCharCode(-1).charCodeAt(0));
r.join(" ");"#;
    assert_eq!(
        value(src),
        "true true false false false true false false true false false true true true true false true false A BC A 65535"
    );
}

/// A polymorphic call whose callee's load is dropped (the cases dispatch
/// on the receiver's shape) keeps it when something between the load and
/// the call may exit (`sym.name`, once a third shape comes): the
/// interpreter would resume past the load and call what the callee's
/// register held before.
#[test]
fn poly_call_keeps_callee_load_before_an_exit() {
    let src = r#"
function T1() { this.n = 0; } T1.prototype.add = function (k, v) { this.n += k.length; return true; };
function T2() { this.m = 0; this.n = 0; } T2.prototype.add = function (k, v) { this.n += 2 * k.length; return true; };
function Box(a, b) { this.a = a; this.b = b; }
function S1(name) { this.name = name; }
function S2(name) { this.x = 1; this.name = name; }
function S3(name) { this.y = 1; this.z = 2; this.name = name; }
function enter(box, pick, sym) {
  var t = pick ? box.a : box.b;
  if (pick === 2) { box.q0 = box.a.n + 0; box.r0 = box.b.n * 0; }
  if (pick === 3) { box.q1 = box.a.n + 1; box.r1 = box.b.n * 1; }
  if (pick === 4) { box.q2 = box.a.n + 2; box.r2 = box.b.n * 2; }
  if (pick === 5) { box.q3 = box.a.n + 3; box.r3 = box.b.n * 3; }
  if (pick === 6) { box.q4 = box.a.n + 4; box.r4 = box.b.n * 4; }
  if (pick === 7) { box.q5 = box.a.n + 5; box.r5 = box.b.n * 5; }
  if (pick === 8) { box.q6 = box.a.n + 6; box.r6 = box.b.n * 6; }
  if (pick === 9) { box.q7 = box.a.n + 7; box.r7 = box.b.n * 7; }
  if (pick === 10) { box.q8 = box.a.n + 8; box.r8 = box.b.n * 8; }
  if (pick === 11) { box.q9 = box.a.n + 9; box.r9 = box.b.n * 9; }
  if (pick === 12) { box.q10 = box.a.n + 10; box.r10 = box.b.n * 10; }
  if (pick === 13) { box.q11 = box.a.n + 11; box.r11 = box.b.n * 11; }

  if (!t.add(sym.name, sym)) return 0;
  return 1;
}
var box = new Box(new T1(), new T2()), s = 0;
for (var i = 0; i < 20000; i++) s += enter(box, i % 2 == 0, i % 3 ? new S1("ab") : new S2("abc"));
for (var i = 0; i < 2000; i++) s += enter(box, i % 2 == 0, new S3("abcd"));
s + " " + box.a.n + " " + box.b.n;"#;
    assert_eq!(value(src), "22000 27334 54666");
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

/// Loads whose caches missed in compiled code (soft: a miss goes through
/// the runtime and on) meeting new shapes, an int32 field holding a double,
/// an array, and a getter (which the runtime leaves to the interpreter).
#[test]
fn soft_loads_agree_with_node() {
    let src = r#"
function P(x) { this.x = x; this.d = x * 0.5; this.o = { k: x }; }
P.prototype.m = function () { return this.x + 1; };
function Q(x) { this.q = 0; this.x = x; this.d = x + 0.25; this.o = { k: -x }; }
Q.prototype.m = function () { return this.x * 2; };
function R(x) { this.r = 0; this.q = 0; this.x = x; this.d = x; this.o = { k: 1 }; }
R.prototype.m = function () { return 7; };
function G(x) { this.g = 0; this.d = 1.5; this.o = { k: 2 }; }
Object.defineProperty(G.prototype, "x", { get: function () { return 100; } });
G.prototype.m = function () { return 3; };
function f(o) { return o.x + o.d + o.o.k + o.m(); }
function main() {
  var objs = [0];
  for (var i = 0; i < 50; i++) objs.push(new P(i));
  var t = 0;
  for (var r = 0; r < 4000; r++) {
    for (var i = 1; i < objs.length; i++) t += f(objs[i]);
    if (r == 1000) for (var i = 0; i < 50; i += 3) objs[i] = new Q(i);
    if (r == 2000) for (var i = 1; i < 50; i += 5) objs[i] = new R(i);
    if (r == 3000) for (var i = 2; i < 50; i += 7) objs[i] = new G(i);
    if (r == 3500) {
      objs[4] = { x: 0.75, d: 1, o: { k: 1 }, m: function () { return 0; } };
      objs[5] = [1, 2]; objs[5].x = 1; objs[5].d = 2; objs[5].o = { k: 3 };
      objs[5].m = function () { return this.length; };
    }
  }
  return t;
}
main();"#;
    assert_eq!(value(src), "16640798");
}

/// A field typed `Num` holds doubles (`object::FieldType::Num`): ints
/// stored before it was become doubles in the objects, stores convert
/// ints (the interpreter's caches, also ones taken while it was `Int`, and
/// the JIT's `_d` stores), and compiled loads take doubles unchecked.
#[test]
fn number_fields_hold_doubles() {
    let src = r#"
function V(x, y) { this.x = x; this.y = y; }
function W(x) { this.y = 0; this.x = x; }
function setx(o, v) { o.x = v; }
function sum(o) { return o.x + o.y; }
function poly(o, v) { o.x = v; return o.x; }
function main() {
  var vs = [];
  for (var i = 0; i < 200; i++) vs.push(new V(i, i + 1));
  var t = 0;
  for (var r = 0; r < 300; r++) {
    for (var i = 0; i < vs.length; i++) t += sum(vs[i]);
    if (r == 100) vs[3].x = 0.5;
    if (r == 150) for (var i = 0; i < vs.length; i += 7) setx(vs[i], i);
    if (r == 200) { delete vs[5].y; vs[5].y = 3; }
  }
  var ws = [new W(1), new W(2.5), new V(1, 2), new V(3, 4)];
  for (var r = 0; r < 2000; r++) t += poly(ws[r & 3], r & 7);
  for (var r = 0; r < 3000; r++) { var n = new V(r, 1); ws[r & 3] = n; t += n.x - n.y; }
  var u = new V(1, 2);
  u.x = 7;
  t += u.x * 3 + vs[3].x + vs[7].x;
  return [t, typeof vs[0].x, vs[14].x === 14, vs[5].y].join();
}
main();"#;
    assert_eq!(value(src), "16501734,number,true,3");
}

/// A load into the slot of the register it loads from (`o = o.o`, as
/// `get_own_nc_g1_to_g1`): on arm64 a family template, whose registers the
/// fused name must give.
#[test]
fn loads_into_their_receivers_slot() {
    let src = r#"
function P(x) { this.x = x; this.o = { k: x }; }
function f(o) { return o.x + o.o.k; }
function main() {
  var objs = [0];
  for (var i = 0; i < 50; i++) objs.push(new P(i));
  var t = 0;
  for (var r = 0; r < 1000; r++) for (var i = 1; i < objs.length; i++) t += f(objs[i]);
  return t;
}
main();"#;
    assert_eq!(value(src), "2450000");
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

/// A direct call whose callee varies (a table of handlers, as gbemu's
/// opcodes): calls of the others through sites of their own in the site's
/// table (`Dispatch`): closures with and without upvalues, made more than
/// once (bound by code), one returning another type now and then, and
/// handlers replaced.
#[test]
fn calls_of_varying_callees_agree_with_node() {
    let src = r#"
function Cpu() { this.a = 0; this.b = 1; this.ops = []; }
function makeOps(cpu, k) {
  var ops = [];
  for (var i = 0; i < 40; i++) {
    if (i % 5 == 0) {
      var cell = { n: i };
      ops.push(function (c) { cell.n++; c.a = (c.a + cell.n + k) | 0; return 1; });
    } else if (i % 7 == 0) {
      ops.push(function (c) { c.b = c.b * 1.5; if (c.b > 1e6) c.b = 1; return 2; });
    } else {
      ops.push(new Function("c", "c.a = (c.a + " + i + ") | 0; return " + (i % 3) + ";"));
    }
  }
  ops[3] = function (c) { return c.a > 100000 ? "big" : 0; };
  ops[9] = function (c) { return ops[1](c) + ops[2](c); };
  return ops;
}
function run(cpu, ops, n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    var r = ops[(i * 7 + (i >> 3)) % ops.length](cpu);
    t += typeof r == "number" ? r : 5;
  }
  return t;
}
function main() {
  var cpu = new Cpu(), t = 0;
  var ops1 = makeOps(cpu, 1);
  for (var k = 0; k < 300; k++) {
    t += run(cpu, ops1, 400);
    if (k == 150) ops1[7] = function (c) { c.a = "s"; return 1; };
    if (k == 151) ops1[7] = function (c) { c.a = 0; return 1; };
    if (k % 100 == 99) ops1 = makeOps(cpu, k);
  }
  return t + " " + cpu.a + " " + Math.round(cpu.b);
}
main();"#;
    assert_eq!(value(src), "150551 34435236 11");
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

/// An array literal's pushes are calls without an inline cache: inlining
/// a function returning one, in a function with no inline caches at all
/// or with some, doesn't read a cache for them (it read past the caches,
/// and crashed).
#[test]
fn inlined_callees_return_array_literals() {
    let src = r#"
function pair(a, b) { return [a, b]; }
function divmod(a, b) { return [(a / b) | 0, a % b]; }
function tagged(o, b) { return [o.k, b]; }
function main() {
  var s = 0, o = {k: 3};
  for (var i = 1; i < 100000; i++) {
    var p = pair(i, 7), q = divmod(i, 7), r = tagged(o, i);
    s += p[0] - p[1] + q[0] - q[1] + r[0] + r[1];
  }
  return s;
}
main();"#;
    assert_eq!(value(src), "10713435724");
}

/// `instanceof` through a cache per site, keyed by the receiver's shape
/// and the function's `prototype` (a new one, or a function of another
/// shape, misses it), and throwing where it should.
#[test]
fn instanceof_cached_by_shape_and_prototype() {
    let src = r#"
function A() {} function B() {} B.prototype = new A(); function C() {}
var objs = [new A(), new B(), new C(), {}, "str", 5, null, [1], new B()];
function count(xs, F) { var n = 0; for (var i = 0; i < xs.length; i++) if (xs[i] instanceof F) n++; return n; }
var t = 0;
for (var r = 0; r < 20000; r++) {
  t += count(objs, A) * 100 + count(objs, B) * 10 + count(objs, C) + count(objs, Object) * 1000 + count(objs, Array) * 10000;
  if (r == 10000) { C.prototype = A.prototype; }
  if (r == 15000) { var fx = function () {}; fx.extra = 1; t += count(objs, fx); }
}
var err = 0; try { count(objs, 3); } catch (e) { err = 1; }
var err2 = 0; try { var g = function () {}; g.prototype = 7; count([{}], g); } catch (e) { err2 = 1; }
t + " " + err + " " + err2;"#;
    assert_eq!(value(src), "326439998 1 1");
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
    assert_eq!(
        value(src),
        "3:6:1:4:10:number,2,3628800,5,3:1/2/3,caught in,localAB,6,5,function|13|1"
    );
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
    assert_eq!(
        value(skimmed),
        "8,2,3:0,pq0,1:9:9,4,4,outer,55,3,1234,2:1,6,2:1,6,3,3"
    );
    // A skimmed body's syntax error is thrown at its first call (Node
    // throws at the `eval`).
    assert_eq!(
        value("var f = (0, eval)('var g = function () { return 1 + ; }; g'); \
               var r; try { f(); } catch (e) { r = String(e).indexOf('SyntaxError') >= 0; } r;"),
        "true"
    );
    // What only the code generator rejects is thrown at the call.
    assert_eq!(
        value("var f = (0, eval)('(function () { return function () { while (1) { continue x; } }; })')(); \
               var r; try { f(); } catch (e) { r = String(e).indexOf('no target') >= 0; } r;"),
        "true"
    );
}

/// Every running microbenchmark through `eval`, so each of its functions
/// is compiled lazily, computes what it does compiled up front.
#[test]
fn microbenchmarks_through_eval_compute_their_expected_results() {
    for f in files("bench/micro", "js") {
        let name = f.file_name().unwrap().to_str().unwrap();
        if !RUNNING.contains(&&number(name)) {
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
    let module = toi::js::compile(src).unwrap();
    for budget in [i64::MAX, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_code_room(64 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r, Ok("13626000".to_string()), "tier at {budget}");
        vm.heap.verify();
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
    let module = toi::js::compile(src).unwrap();
    for budget in [i64::MAX, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        vm.set_gc_verify(64 << 10);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        assert_eq!(r, Ok("47616".to_string()), "tier at {budget}");
        vm.heap.verify();
    }
}

/// A field type generalized by a store in the interpreter drops the code
/// of callers bound to a version that took the old type, not only that
/// version's (`Vm::rely_through`). Mandreel's `malloc`, reduced: its
/// version read global `tlsf_ptr` as an int32; the interpreter stored a
/// double there after an exit, and the loop's direct call kept entering
/// that version, which read the double's low half (0) and set up the heap
/// again. The expected value is Node's.
#[test]
fn field_types_generalized_unbind_direct_callers() {
    let src = r#"
var heap = new ArrayBuffer(1 << 16), heap32 = new Int32Array(heap), heapU32 = new Uint32Array(heap);
var r_g0 = 0, tlsf_ptr = 0, next = 0, bad = "";
function tlsf_create(sp) { next = 1000; r_g0 = 4096; }
function tlsf_malloc(sp) { var p = next; next = (next + heap32[(sp + 4) >> 2] + 7) & ~7; r_g0 = p; }
function setD() { r_g0 = 11856.5; }
function malloc(sp) {
  var size = heapU32[sp >> 2];
  if (size == 0) { size = 4; }
  if (tlsf_ptr == 0) {
    var addr = 77;
    sp -= 8;
    heap32[(sp) >> 2] = addr;
    heap32[(sp + 4) >> 2] = 1234;
    tlsf_create(sp);
    tlsf_ptr = r_g0;
  }
  sp -= 8;
  heap32[(sp) >> 2] = tlsf_ptr;
  heap32[(sp + 4) >> 2] = size;
  tlsf_malloc(sp);
  if (r_g0 == 0) { bad = bad || "malloc failed"; }
  return;
}
function user(sp, n) {
  var s = 0;
  for (var i = 0; i < n; i++) { heap32[sp >> 2] = 16; malloc(sp); var r5 = r_g0; if (r5 != 0) { s = (s + (r5 & 15)) | 0; } }
  return s;
}
var res = [];
for (var it = 0; it < 4; it++) {
  tlsf_ptr = 0; setD();
  res.push(user(512, 20000), tlsf_ptr, next);
}
bad || res.join();
"#;
    assert_eq!(
        value(src),
        "160000,4096,321000,160000,4096,321000,160000,4096,321000,160000,4096,321000"
    );
}

/// `Math.sqrt` and `Math.pow` compiled inline (`Insn::native` 6 and 7):
/// the argument in `acc` (live into the call), `pow`'s base read from
/// its own register; strings and objects leave for the interpreter. The
/// expected value is Node's.
#[test]
fn math_sqrt_and_pow_compile_inline() {
    let src = r#"
var o = { g: 2 };
function f(o, y) {
  var a = Math.pow(10, o.g + 1);
  var b = Math.pow(Math.max(y, 0), o.g);
  var c = 0.5 * Math.pow(y, 0.5);
  var d = (-y) - Math.sqrt(y * 2.5);
  return a + b + c + d;
}
function V(x, y) { this.x = x; this.y = y; }
V.prototype.magnitude = function () { return Math.sqrt(this.x * this.x + this.y * this.y); };
var s = 0, m = 0;
for (var i = 0; i < 30000; i++) { s += f(o, i % 9); m += 1 / new V(i % 7, 1.5).magnitude(); }
[s, m, f(o, "4"), Math.sqrt("x"), Math.pow({ valueOf: function () { return 3; } }, 2), Math.pow(-8, 1 / 3)].join();
"#;
    assert_eq!(
        value(src),
        "30501185.765135095,10743.993963615587,1009.8377223398317,NaN,9,NaN"
    );
}

/// `Math.floor`, `ceil` and `round` results predicted int32 (checked
/// after the call); a call giving a double, NaN or -0 leaves with it, and
/// is predicted no more. The expected value is Node's.
#[test]
fn math_floor_results_predicted_int32() {
    let src = r#"
function f(x) { var a = Math.floor(x) + 1, b = Math.ceil(x) | 0, c = Math.round(x) * 2; return a + b + c; }
var s = 0;
for (var i = 0; i < 30000; i++) s += f(i * 0.75);
var t = [f(3e10 + 0.5), f(-0.2), f(NaN), f(-2.5), f("7.5")];
for (var i = 0; i < 30000; i++) s += f(i * 1e6 + 0.25);
[s].concat(t).join();
"#;
    assert_eq!(value(src), "1349961921417876,89935228932,0,NaN,-8,32");
}

/// Truthiness in compiled code (`jtotrue`/`jtofalse`): objects of every
/// class are true without the runtime, strings by their length. The
/// expected value is Node's.
#[test]
fn truthiness_of_every_kind_of_value() {
    let src = r#"
var vals = [{}, [], "", "a", "a" + "b", function () {}, new Date(0), 0.5, -0, NaN, null, undefined, true, false, 0, 7, new ArrayBuffer(0), /x/];
function count(v) { var n = 0; for (var k = 0; k < 3; k++) { if (v) n++; if (!v) n += 10; } return n; }
var out = [];
for (var r = 0; r < 2000; r++) for (var i = 0; i < vals.length; i++) out[i] = count(vals[i]);
out.join();
"#;
    assert_eq!(value(src), "3,3,30,3,3,3,3,3,30,30,30,30,3,30,30,3,3,3");
}

/// `Math.max` and `Math.min` of two compiled inline: ints, doubles, NaN,
/// -0 and +0, and arguments to convert (a string, an object's `valueOf`),
/// for which the code leaves. The expected value is Node's.
#[test]
fn math_max_and_min_compile_inline() {
    let src = r#"
function f(a, b) { return Math.max(a, b) + "/" + Math.min(a, b) + "/" + (1 / Math.max(a, b)) + "/" + (1 / Math.min(a, b)); }
function g(a, b) { var s = 0; for (var i = 0; i < 1000; i++) s += Math.max(a + i, b) - Math.min(a, b - i); return s; }
var s = 0;
for (var i = 0; i < 30000; i++) s += g(i % 7, 3.5) + g(1.25, i % 5);
var o = { valueOf: function () { return 4; } };
var t = [f(1, 2), f(2.5, -1), f(-0, 0), f(0, -0), f(NaN, 1), f(1, NaN), f(3, 3), f("5", 2), f(o, 1), f(-Infinity, 1e308), f(7, 7.5)];
var q = { m: 0 }; for (var i = 0; i < 20000; i++) q.m = Math.max(q.m, i % 100);
var r = 0; for (var i = 0; i < 20000; i++) r += [f(1, 2), f(-0, 0), f(o, 1)].length;
[s, q.m, r].concat(t).join();
"#;
    assert_eq!(
        value(src),
        "59902725580,99,60000,2/1/0.5/1,2.5/-1/0.4/-1,0/0/Infinity/-Infinity,0/0/Infinity/-Infinity,\
         NaN/NaN/NaN/NaN,NaN/NaN/NaN/NaN,3/3/0.3333333333333333/0.3333333333333333,5/2/0.2/0.5,\
         4/1/0.25/1,1e+308/-Infinity/1e-308/0,7.5/7/0.13333333333333333/0.14285714285714285"
    );
}

/// Prototype's `Class.create` constructors (`this.initialize.apply(this,
/// arguments)`) inlined at any depth, their `initialize` within the limit:
/// a `new` in an inlined `initialize`, then `initialize` replaced. The
/// expected value is Node's.
#[test]
fn class_create_constructors_inline_nested() {
    let src = r#"
var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var Color = Class.create();
Color.prototype = { initialize: function (r, g, b) { if (!r) r = 0.0; if (!g) g = 0.0; if (!b) b = 0.0; this.red = r; this.green = g; this.blue = b; } };
var Info = Class.create();
Info.prototype = { initialize: function () { this.color = new Color(0, 0, 0); this.hit = false; } };
var Shape = Class.create();
Shape.prototype = { initialize: function (k) { this.k = k; }, intersect: function (n) { var info = new Info(); if (n % 3) { info.hit = true; info.color = new Color(n, this.k, arguments.length); } return info; } };
var sh = new Shape(2), s = 0;
for (var i = 0; i < 50000; i++) { var r = sh.intersect(i); s += r.color.red + r.color.green * 2 + r.color.blue * 3 + (r.hit ? 1 : 0); }
Color.prototype.initialize = function (r, g, b) { this.red = r * 10; this.green = g; this.blue = b; };
for (var i = 0; i < 1000; i++) { var r = sh.intersect(i); s += r.color.red + r.color.green * 2 + r.color.blue * 3 + (r.hit ? 1 : 0); }
var x = sh.intersect(4);
[s, x.color.red, x.color.green, x.color.blue, x.hit].join();
"#;
    assert_eq!(value(src), "836915329,40,2,1,true");
}

/// Constructor code several classes share (Prototype's `Class.create`)
/// tracks each prototype's capacity: a class with properties added after
/// its constructor, one inside it, and one whose `initialize` adds more
/// later. The expected value is Node's.
#[test]
fn shared_constructor_capacity_per_prototype() {
    let src = r#"
var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var P = Class.create(); P.prototype = { initialize: function (x, y) { this.x = x; this.y = y; } };
var Big = Class.create(); Big.prototype = { initialize: function () { this.p = new P(1, 2); } };
function hit(n) { var b = new Big(); b.a = n; b.b = n * 2; b.c = n + 0.5; b.d = "s" + (n % 3); b.e = b.p.x + b.p.y; return b; }
var s = 0, t = "";
for (var i = 0; i < 40000; i++) { var b = hit(i); s += b.a + b.b + b.c + b.e + b.p.x * i; t = b.d; }
P.prototype.initialize = function (x, y) { this.x = x; this.y = y; this.z = x * y; this.w = 7; };
for (var i = 0; i < 2000; i++) { var q = new P(i, 3); s += q.z + q.w + q.x; var b = hit(i); s += b.p.z + b.p.w; }
var last = hit(9);
[s, t, last.a, last.b, last.c, last.d, last.e, last.p.z, last.p.w, Object.keys(last).join("")].join();
"#;
    assert_eq!(value(src), "4008068000,s0,9,18,9.5,s0,3,2,7,pabcde");
}

/// Getters and setters in prototypes, inlined or called from compiled code
/// (`vm::ACCESSOR_LOC`): receivers of several shapes, a setter nested
/// past the inlining depth, one writing its parameter, an assignment's
/// value used after a setter, an own accessor shadowing, a redefinition.
#[test]
fn accessors_compile_inline_or_called() {
    let src = r#"
function Id() { this.f = { a: 0, b: 0 }; }
Object.defineProperty(Id.prototype, "key", { configurable: true, get: function () { return this._key; } });
Object.defineProperty(Id.prototype, "key", { configurable: true, set: function (v) {
  if (v === undefined) v = 0;
  this._key = v; this.f.a = v & 255; this.f.b = (v >> 8) & 255;
} });
function Holder() { this.id = new Id(); }
Holder.prototype.Set = function (o) { this.id.key = o.id.key; };
function Pair() { this.h = new Holder(); this.g = new Holder(); }
Pair.prototype.Copy = function (b) { this.h.Set(b.h); this.g.Set(b.g); };
function chain(x) { var y; y = x.key = 7; return y; }
function main() {
  var t = 0, ps = [], i;
  for (i = 0; i < 40; i++) { var p = new Pair(); p.h.id.key = i * 3; p.g.id.key = i * 257; ps.push(p); }
  for (var r = 0; r < 400; r++) {
    for (i = 1; i < ps.length; i++) {
      ps[i].Copy(ps[i - 1]);
      t += ps[i].h.id.key + ps[i].g.id.f.b;
    }
    ps[0].h.id.key = r; ps[0].g.id.key = r * 300;
    if (r == 200) ps[5].g.id.key = 1.5;         // a double through the setter
    if (r == 250) ps[6].h.id.key = undefined;   // the setter writes its parameter
    if (r == 300) { var s = new Id(); s.key = 5; s.extra = 1; ps[0].h.id = s; }   // another receiver shape
  }
  var o = new Id();
  t += chain(o) + o.key;
  // An own accessor shadowing the prototype's, then a redefinition.
  Object.defineProperty(ps[3].h.id, "key", { configurable: true, get: function () { return 1000; } });
  for (i = 0; i < 300; i++) t += ps[i % 40].h.id.key;
  Object.defineProperty(Id.prototype, "key", { configurable: true, get: function () { return -1; } });
  for (i = 0; i < 300; i++) t += ps[i % 40].h.id.key;
  return t;
}
main();"#;
    assert_eq!(value(src), "5041227");
}

/// An exit at a setter's receiver check resumes at the store, with the
/// value in `acc` (the check reads it: `Insn::store_check`).
#[test]
fn exits_at_setter_checks_keep_the_value() {
    let src = r#"
function Id() { this.f = 0; }
Object.defineProperty(Id.prototype, "key", { set: function (v) { this._key = v; } });
function main() {
  var a = new Id(), b = new Id(); b.extra = 1;
  var c = new Id(); c.other = 2; c.more = 3;
  var objs = [a, b], out = [];
  for (var r = 0; r < 60; r++) {
    var o = objs[r & 1];
    if (r == 50) o = c;
    o.key = r;
    out.push(o._key);
  }
  return out.join(",");
}
main();"#;
    let expect: Vec<String> = (0..60).map(|r| r.to_string()).collect();
    assert_eq!(value(src), expect.join(","));
}

/// Stores to a function's fields given up to the runtime (`set_named_rt`):
/// a counter never set (`NaN`, stored again and again, its field constant),
/// a constructor's `prototype` replaced.
#[test]
fn compiled_stores_to_function_fields() {
    let src = r#"
var B = {};
function W() {}
B.W = W;
W.Distance = function (x) { ++W.calls; var t = 0; for (var i = 0; i < 20; i++) t += x * i; ++W.iters; W.last = x; return t; };
W.iters = 0;
function P() {}
var protos = [{ k: 1 }, { k: 2 }];
function make(r) { P.prototype = protos[r >> 9 & 1]; return new P().k; }
function main() {
  var t = 0;
  for (var r = 0; r < 3000; r++) t += W.Distance(r & 7) + make(r);
  return [t, W.calls, W.iters, W.last].join();
}
main();"#;
    assert_eq!(value(src), "1999464,NaN,3000,7");
}

/// A small loop-free function whose registers its closures capture,
/// recursing through them (EarleyBoyer's `deriv_trees`): it counts down
/// to compiling, and a compiled caller's site binds once it has
/// (`CallSite::wait_hot`). And `typeof` of each kind of value (its
/// strings made once).
#[test]
fn capturing_drivers_compile_and_bind_when_hot() {
    let src = r#"
function Pair(car, cdr) { this.car = car; this.cdr = cdr; }
var trees;
function make(depth) {
  trees = function (n, k) {
    var loop;
    if (n <= 0) return new Pair(k, null);
    loop = function (i, acc) {
      while (i > 0) {
        var t = trees(n - 1, k + i);
        acc = new Pair(t.car + acc.car, acc);
        i--;
      }
      return acc;
    };
    return loop(2, new Pair(n, null));
  };
}
function kinds(v) { return typeof v; }
function main() {
  make();
  var t = 0;
  for (var r = 0; r < 3000; r++) t += trees(4, r & 7).car;
  var ks = [1, 1.5, undefined, true, "s", main, {}, null, Math.max].map(kinds).join();
  return t + ":" + ks;
}
main();"#;
    assert_eq!(
        value(src),
        "534000:number,number,undefined,boolean,string,function,object,object,function"
    );
}
