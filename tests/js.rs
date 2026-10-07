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
    "01", "02", "03", "04", "05", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "17",
    "20", "21", "22", "23", "24", "25", "26", "27", "30", "31", "32", "33", "34", "35", "36", "37",
    "40", "41", "42", "43", "44", "45", "50", "51", "52", "53", "60", "61", "62", "63", "64", "65",
    "66", "67", "68", "69", "70", "71", "80", "81", "82", "83", "84", "90", "91", "92", "93", "94",
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

#[test]
fn rejects_what_it_cannot_compile() {
    let unsupported = [
        (
            "function f(a) { return function () { return a; }; } f(1);",
            "captured",
        ),
        ("var g = function h() { return h; };", "own name"),
        ("function f(i) { return i++; }", "postfix"),
        ("var x = typeof 1;", "Typeof"),
        ("var o = {1: 2};", "the key `1`"),
        ("function f(a, b) { return a % b; }", "`%`"),
        ("switch (1) {}", "switch"),
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
