//! Issue #272: length is a property lookup, not a typed-array header load.
use cloison::vm::{Vm, display};

#[test]
fn typed_array_length_uses_property_descriptors() {
    let src = include_str!("fixtures/typed-array-length.js");
    let module = cloison::js::compile(src).unwrap();
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let result = vm.run(&module).unwrap_or_else(|e| panic!("tier {budget}: {e}"));
        assert_eq!(display(result), "ok", "tier {budget}");
        vm.verify_heap();
    }
}

#[test]
fn host_length_lookup_respects_virtual_ancestors() {
    let mut vm = Vm::new();
    let setup = cloison::js::compile(
        "var ta = new Uint8Array(3), p = [1, 2]; \
         Object.setPrototypeOf(p, {length: 99}); \
         Object.setPrototypeOf(ta, p); ta",
    )
    .unwrap();
    let ta = vm.run(&setup).unwrap();
    let length = vm.intern("length");
    assert_eq!(display(vm.get(ta, length).unwrap()), "2");
    vm.verify_heap();
}
