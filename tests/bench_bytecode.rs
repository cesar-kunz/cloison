//! Runs the hand-written listings in `bench/bytecode/` and checks each
//! result against the `// expect:` line of the benchmark it compiles,
//! `bench/micro/<same name>.js`, in the interpreter and through the JIT.

use std::path::Path;

use toi::vm::{Vm, display};

#[test]
fn listings_match_their_benchmarks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("bench");
    let mut files: Vec<_> = std::fs::read_dir(root.join("bytecode"))
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "asm"))
        .collect();
    files.sort();
    assert!(!files.is_empty());
    for f in files {
        let js = root
            .join("micro")
            .join(f.with_extension("js").file_name().unwrap());
        let js = std::fs::read_to_string(&js).unwrap_or_else(|e| panic!("{}: {e}", js.display()));
        let expect = js
            .lines()
            .find_map(|l| l.strip_prefix("// expect: "))
            .unwrap_or_else(|| panic!("{}: no expect line", f.display()));
        let module = toi::asm::parse(&std::fs::read_to_string(&f).unwrap())
            .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
        // Interpreter only, tiering up after 1 and 2 back edges, and the
        // default.
        for budget in [i64::MAX, 1, 2, 1000] {
            let mut vm = Vm::new();
            vm.set_tier_back_edges(budget);
            let v = vm
                .run(&module)
                .unwrap_or_else(|e| panic!("{}: {e}", f.display()));
            assert_eq!(
                display(v),
                expect.trim(),
                "{} (tier at {budget})",
                f.display()
            );
        }
    }
}

fn listing(name: &str) -> toi::asm::Module {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("bench/bytecode")
        .join(name);
    toi::asm::parse(&std::fs::read_to_string(path).unwrap()).unwrap()
}

#[test]
fn hot_property_loops_compile_without_repeated_guards() {
    // 34: the loop only replaces fields of `c`, whose shape is known from
    // entry, so none of its accesses is guarded.
    let mut vm = Vm::new();
    vm.run(&listing("34-chained-loads.asm")).unwrap();
    let s = vm.jit_stencils("main").expect("main's loop compiled");
    assert!(s.iter().any(|n| n.starts_with("get_own_nc_")), "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("set_own_nc_")), "{s:?}");
    // 21: `v = vs[i]` is guarded once, then known.
    let mut vm = Vm::new();
    vm.run(&listing("21-monomorphic-get.asm")).unwrap();
    let s = vm.jit_stencils("main").expect("main's loop compiled");
    assert!(s.iter().any(|n| n.starts_with("get_keyed_i_")), "{s:?}");
    let guarded = s
        .iter()
        .filter(|n| n.starts_with("get_own_") && !n.contains("_nc_"))
        .count();
    assert_eq!(guarded, 1, "{s:?}");
}
