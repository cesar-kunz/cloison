//! The scripts under `tests/scripts/`, one test each, named by their path
//! there. A script starts with comments saying what it shows, then one
//! that says what it must give:
//!
//! - `// expect: V`: it completes with the value `V` (as `display` shows it);
//! - `// error: E`: it throws, or doesn't compile, with the message `E`;
//! - `// compile error: E`: it doesn't compile, with the message `E`.
//!
//! With `-json` after the word (`// expect-json: "a\nb"`), what follows is
//! a JSON string, for a result with line breaks, edge spaces or characters
//! that don't show. The script is what follows that line, exactly. It runs
//! with the interpreter only, tiering up after 1, 2 and 1000 back edges and
//! with the default, each in a new `Vm` whose heap is verified at the end;
//! all must agree.

use std::path::{Path, PathBuf};

use cloison::vm::{Vm, display};
use libtest_mimic::{Arguments, Failed, Trial};

enum Expect {
    Value(String),
    Error(String),
    CompileError(String),
}

/// The expectation and the script of a test file.
fn parse(text: &str) -> Result<(Expect, &str), String> {
    let mut rest = text;
    while rest.starts_with("//") {
        let (line, after) = rest.split_once('\n').unwrap_or((rest, ""));
        let directive = line.strip_prefix("// ").and_then(|l| l.split_once(':'));
        if let Some((word, arg)) = directive {
            let (word, json) = match word.strip_suffix("-json") {
                Some(w) => (w, true),
                None => (word, false),
            };
            if matches!(word, "expect" | "error" | "compile error") {
                let arg = arg.strip_prefix(' ').unwrap_or(arg);
                let arg = if json { unjson(arg)? } else { arg.to_string() };
                let expect = match word {
                    "expect" => Expect::Value(arg),
                    "error" => Expect::Error(arg),
                    _ => Expect::CompileError(arg),
                };
                return Ok((expect, after));
            }
        }
        rest = after;
    }
    Err("no `// expect:`, `// error:` or `// compile error:` line in the leading comments".into())
}

/// A JSON string's value.
fn unjson(s: &str) -> Result<String, String> {
    let bad = || format!("not a JSON string: {s}");
    let mut chars = s
        .trim_end()
        .strip_prefix('"')
        .and_then(|s| s.strip_suffix('"'))
        .ok_or_else(bad)?
        .chars();
    let mut out = String::new();
    let hex4 = |chars: &mut std::str::Chars| -> Option<u32> {
        u32::from_str_radix(&chars.by_ref().take(4).collect::<String>(), 16).ok()
    };
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        let c = match chars.next().ok_or_else(bad)? {
            'n' => '\n',
            'r' => '\r',
            't' => '\t',
            'b' => '\u{8}',
            'f' => '\u{c}',
            'u' => {
                let hi = hex4(&mut chars).ok_or_else(bad)?;
                let code = if (0xD800..0xDC00).contains(&hi) {
                    let lo = (chars.next() == Some('\\') && chars.next() == Some('u'))
                        .then(|| hex4(&mut chars))
                        .flatten()
                        .ok_or_else(bad)?;
                    0x10000 + ((hi - 0xD800) << 10) + (lo - 0xDC00)
                } else {
                    hi
                };
                char::from_u32(code).ok_or_else(bad)?
            }
            c => c,
        };
        out.push(c);
    }
    Ok(out)
}

/// What `src` gives, the same at every tier budget.
fn run(src: &str) -> Result<Result<String, String>, String> {
    let module = match cloison::js::compile(src) {
        Ok(m) => m,
        Err(e) => return Ok(Err(e)),
    };
    let mut result = None;
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        vm.verify_heap();
        match &result {
            None => result = Some(r),
            Some(first) if *first != r => {
                return Err(format!(
                    "tiering up after {budget} back edges gives {r:?}, without {first:?}"
                ));
            }
            Some(_) => {}
        }
    }
    Ok(result.unwrap())
}

fn check(path: &Path) -> Result<(), Failed> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let (expect, src) = parse(&text)?;
    let got = match expect {
        Expect::CompileError(_) => Err(cloison::js::compile(src)
            .err()
            .unwrap_or_else(|| "(compiles)".into())),
        _ => run(src)?,
    };
    let ok = match (&expect, &got) {
        (Expect::Value(v), Ok(g)) => v == g,
        (Expect::Error(e) | Expect::CompileError(e), Err(g)) => e == g,
        _ => false,
    };
    if ok {
        return Ok(());
    }
    let want = match expect {
        Expect::Value(v) => format!("the value {v:?}"),
        Expect::Error(e) => format!("the error {e:?}"),
        Expect::CompileError(e) => format!("the compile error {e:?}"),
    };
    let got = match got {
        Ok(v) => format!("the value {v:?}"),
        Err(e) => format!("the error {e:?}"),
    };
    Err(format!("expected {want}, got {got}").into())
}

fn scripts(dir: &Path, out: &mut Vec<PathBuf>) {
    for e in std::fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.is_dir() {
            scripts(&p, out);
        } else if p.extension().is_some_and(|e| e == "js") {
            out.push(p);
        }
    }
}

fn main() {
    let args = Arguments::from_args();
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/scripts");
    let mut paths = Vec::new();
    scripts(&dir, &mut paths);
    paths.sort();
    let trials = paths
        .into_iter()
        .map(|p| {
            let name = p
                .strip_prefix(&dir)
                .unwrap()
                .with_extension("")
                .to_string_lossy()
                .into_owned();
            Trial::test(name, move || check(&p))
        })
        .collect();
    libtest_mimic::run(&args, trials).exit();
}
