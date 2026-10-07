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
    "01", "08", "10", "16", "17", "21", "22", "23", "25", "32", "33", "34", "35", "36", "37", "44",
    "50", "51", "53", "70", "90", "91", "92", "93", "94",
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
        ("var s = 'a';", "strings"),
        ("var x = 1.5;", "1.5"),
        ("var o = {};", "object literals"),
        ("function f(a, b) { return a * b; }", "`*`"),
        ("switch (1) {}", "switch"),
    ];
    for (src, what) in unsupported {
        let e = compile_error(src);
        assert!(e.contains("unsupported") && e.contains(what), "{src}: {e}");
    }
    assert!(compile_error("var a = 1;\nvar b = ;").starts_with("2:9:"));
    assert!(compile_error("while (1) { continue x; }").contains("no target"));
}
