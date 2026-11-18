//! Issue #272: length is a property lookup, not a typed-array header load.
use cloison::vm::{Vm, display};

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
    assert_eq!(display(ta.get(&mut vm, length).unwrap(), &vm), "2");
    vm.verify_heap();
}
