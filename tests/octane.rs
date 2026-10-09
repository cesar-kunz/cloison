//! The language features and builtins Octane's benchmarks need beyond
//! Richards and DeltaBlue, one small script each in `tests/octane/`: its
//! `// for:` line names the benchmarks that need it, its `// expect:` line
//! is Node's completion value. `scripts/octane.sh` runs the benchmarks
//! themselves.

use std::path::{Path, PathBuf};

use toi::vm::{Vm, display};

/// The scripts toi runs to Node's value (by number); the rest don't yet.
/// Add a script's number here when its feature lands.
const PASSING: &[&str] = &["01", "02", "03", "04", "05", "06", "07", "08", "09", "10", "11", "12", "13", "14", "15", "16", "18", "19", "20"];

fn scripts() -> Vec<PathBuf> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/octane");
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "js"))
        .collect();
    v.sort();
    v
}

/// `src`'s value with the interpreter only, tiering up after 1 and 2 back
/// edges, and with the default; all must agree.
fn run(name: &str, src: &str) -> Result<String, String> {
    let module = toi::js::compile(src)?;
    let mut result = None;
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&module).map(display).map_err(|e| e.to_string());
        match &result {
            None => result = Some(r),
            Some(first) => assert_eq!(first, &r, "{name}: tier at {budget}"),
        }
    }
    result.unwrap()
}

#[test]
fn octane_features_agree_with_node() {
    let mut passing = Vec::new();
    let mut failing = String::new();
    for f in scripts() {
        let name = f.file_name().unwrap().to_str().unwrap();
        let src = std::fs::read_to_string(&f).unwrap();
        let expect = src
            .lines()
            .find_map(|l| l.strip_prefix("// expect: "))
            .unwrap_or_else(|| panic!("{name}: no `// expect:` line"))
            .trim();
        let got = run(name, &src);
        if got.as_deref() == Ok(expect) {
            passing.push(name[..2].to_string());
        } else if PASSING.contains(&&name[..2]) {
            failing += &format!("{name}: {got:?}, expected {expect:?}\n");
        }
    }
    assert!(failing.is_empty(), "{failing}");
    assert_eq!(passing, PASSING, "update PASSING");
}
