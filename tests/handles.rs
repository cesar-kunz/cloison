//! Handle scopes (#369): values the host holds rooted until the scope
//! ends, across runs, operations and moving collections.

use cloison::vm::{Global, Local, Vm};
use std::panic::{AssertUnwindSafe, catch_unwind};

fn js(src: &str) -> cloison::asm::Checked {
    cloison::js::compile(src).unwrap()
}

/// JavaScript that allocates, and collects in full twice.
const CHURN: &str = "for (var i = 0; i < 2e5; i++) var x = {i: [i]}; $262.gc(); $262.gc();";

/// An object only a scoped local holds is moved by an evacuating
/// collection (its address before and after, the instrumentation), and
/// read where it went: its fields, and its identity (a function marks
/// it, the local reads the mark). The slots' vector reallocates
/// meanwhile, many times. Under each way of evacuating: a full
/// collection's, and a concurrent cycle's.
#[test]
fn a_scoped_local_follows_its_object_as_it_moves() {
    for cycles in [false, true] {
        let mut vm = Vm::new();
        if cycles {
            vm.set_gc_evacuate_cycles();
        } else {
            vm.set_gc_evacuate_all();
        }
        vm.with_handles(|s| {
            let o = s.run(&js("({tag: 'held', n: [1, 2, 3]})")).unwrap();
            let at = s.unrooted(o).to_bits();
            // (Slots enough to reallocate the vector, from its few: each
            // `root` a slot.)
            let start = s.vm().handle_depth();
            for i in 0..100_000 {
                s.root(Local::from(i));
            }
            assert!(s.vm().handle_depth() >= start + 100_000);
            for _ in 0..3 {
                s.run(&js(CHURN)).unwrap();
            }
            assert_ne!(s.unrooted(o).to_bits(), at, "cycles {cycles}: not moved");
            let (tag, n, mark) = (s.intern("tag"), s.intern("n"), s.intern("mark"));
            let t = s.get(o, tag).unwrap();
            assert_eq!(s.display(t), "held");
            let list = s.get(o, n).unwrap();
            let two = s.root(Local::from(2));
            let third = s.get_keyed(list, two).unwrap();
            assert_eq!(s.to_number(third).unwrap(), 3.0);
            let f = s.run(&js("(function (o) { o.mark = 'marked'; })")).unwrap();
            let undefined = s.root(Local::UNDEFINED);
            // (`call` runs a native's alone: `Reflect.apply`.)
            let apply = s.run(&js("Reflect.apply")).unwrap();
            let args = s.array();
            let zero = s.root(Local::from(0));
            s.set_keyed(args, zero, o).unwrap();
            s.call(apply, undefined, &[f, undefined, args]).unwrap();
            let m = s.get(o, mark).unwrap();
            assert_eq!(s.display(m), "marked");
        });
        vm.verify_heap();
        assert_eq!(vm.handle_depth(), 0);
    }
}

/// Values of a scope stay valid through runs and operations that run
/// JavaScript, which ends every `Local` (#365): many live at once.
#[test]
fn scoped_locals_outlive_operations() {
    let mut vm = Vm::new();
    vm.with_handles(|s| {
        let a = s.run(&js("({get x() { for (var i = 0; i < 1e5; i++) var t = {i: [i]}; return 'X'; }})")).unwrap();
        let b = s.string("b").unwrap();
        let x = s.intern("x");
        let got = s.get(a, x).unwrap();
        s.run(&js(CHURN)).unwrap();
        let again = s.get(a, x).unwrap();
        assert_eq!((s.display(got), s.display(again), s.display(b)), ("X".into(), "X".into(), "b".into()));
        // A `Local` of the API before, rooted, and given back.
        let l = s.unrooted(b);
        let r = s.root(l);
        s.run(&js("1")).unwrap();
        assert_eq!(s.string_of(r).as_deref(), Some("b"));
    });
}

/// Nested scopes drop their values as they end, on return and on
/// unwinding; `escape_scope` keeps one in the outer scope, in a slot
/// below the inner one's; the outer scope's values are the inner's
/// through `Lift`.
#[test]
fn nested_scopes_release_their_roots() {
    let mut vm = Vm::new();
    vm.with_handles(|s| {
        let make = js("({n: 1})");
        let o = s.run(&make).unwrap();
        let depth = s.vm().handle_depth();
        let n = s.intern("n");
        for round in 0..1000 {
            s.scope(|inner, up| {
                let o = up.lift(o);
                let v = inner.get(o, n).unwrap();
                let w = inner.run(&make).unwrap();
                assert_eq!(inner.to_number(v).unwrap(), 1.0, "{round}");
                let _ = w;
            });
            assert_eq!(s.vm().handle_depth(), depth);
        }
        let kept = s
            .escape_scope(|inner, up| {
                for _ in 0..100 {
                    inner.run(&make).unwrap();
                }
                inner.get(up.lift(o), n)
            })
            .unwrap();
        assert_eq!(s.vm().handle_depth(), depth + 1);
        s.run(&js(CHURN)).unwrap();
        assert_eq!(s.display(kept), "1");
        // What an inner scope threw, its value with it. (The run's
        // value above took a slot too.)
        let before = s.vm().handle_depth();
        let e = s.escape_scope(|inner, _| inner.run(&js("throw {why: 'inner'}"))).unwrap_err();
        assert_eq!(s.vm().handle_depth(), before + 1);
        let why = s.intern("why");
        let w = s.get(e.thrown().unwrap(), why).unwrap();
        assert_eq!(s.display(w), "inner");
        // Unwinding out of an inner scope, caught in the outer.
        let before = s.vm().handle_depth();
        let r = catch_unwind(AssertUnwindSafe(|| {
            s.scope(|inner, _| {
                for _ in 0..10 {
                    inner.run(&make).unwrap();
                }
                panic!("unwinding");
            })
        }));
        assert!(r.is_err());
        assert_eq!(s.vm().handle_depth(), before);
        let again = s.get(o, n).unwrap();
        assert_eq!(s.display(again), "1");
    });
    assert_eq!(vm.handle_depth(), 0);
    // And out of the outer one: the VM goes on, no root left.
    let r = catch_unwind(AssertUnwindSafe(|| {
        vm.with_handles(|s| {
            s.run(&js("[1, 2, 3]")).unwrap();
            panic!("unwinding");
        })
    }));
    assert!(r.is_err());
    assert_eq!(vm.handle_depth(), 0);
    assert_eq!(cloison::vm::display(vm.run(&js("1 + 1")).unwrap(), &vm), "2");
}

/// The values a scope held are let go as it ends: a collection then frees
/// them (`WeakRef`s tell), where it kept them while it was open.
#[test]
fn a_scope_ends_its_roots() {
    let mut vm = Vm::new();
    vm.run(&js("var refs = [];")).unwrap();
    let make = js("(function () { var o = {big: new Array(1000).fill(1)}; refs.push(new WeakRef(o)); return o; })()");
    let alive = js("$262.gc(); $262.gc(); refs.filter(function (r) { return r.deref() !== undefined; }).length");
    let count = |vm: &mut Vm| cloison::vm::display(vm.run(&alive).unwrap(), vm).parse::<u32>().unwrap();
    vm.with_handles(|s| {
        for _ in 0..50 {
            s.run(&make).unwrap();
        }
        let n = s.run(&alive).unwrap();
        assert_eq!(s.display(n), "50");
    });
    let left = count(&mut vm);
    // (0 here, under each `CLOISON_GC_*` and with the JIT off: the native
    // stack is scanned conservatively, so a few may stay.)
    assert!(left < 25, "{left} of 50 alive");
}

/// A `Global` is the way out of a scope and back into another; another
/// VM's is refused, even where its slot number is one of this VM's.
#[test]
fn globals_cross_scopes_of_their_vm_only() {
    let mut vm = Vm::new();
    let mut other = Vm::new();
    let kept: Global = vm.with_handles(|s| {
        let o = s.run(&js("({k: 'v'})")).unwrap();
        s.persist(o)
    });
    let theirs = other.with_handles(|s| {
        let o = s.run(&js("({k: 'theirs'})")).unwrap();
        s.persist(o)
    });
    vm.run(&js(CHURN)).unwrap();
    vm.with_handles(|s| {
        let o = s.local(&kept);
        let k = s.intern("k");
        let v = s.get(o, k).unwrap();
        assert_eq!(s.display(v), "v");
        assert!(catch_unwind(AssertUnwindSafe(|| s.local(&theirs))).is_err());
    });
    drop(other);
    vm.with_handles(|s| assert!(catch_unwind(AssertUnwindSafe(|| s.local(&theirs))).is_err()));
    // A stale `Local` is refused as it would be anywhere (#365).
    let stale = vm.global_object();
    vm.run(&js("1")).unwrap();
    vm.with_handles(|s| assert!(catch_unwind(AssertUnwindSafe(|| s.root(stale))).is_err()));
}

/// What a scope's operation throws is a value of the scope's, rooted:
/// read after more JavaScript and a moving collection, its full message
/// made from it at any time in the scope, and an owned `Error` out of it.
#[test]
fn thrown_values_are_scoped_locals() {
    let mut vm = Vm::new();
    vm.set_gc_evacuate_all();
    let error = vm.with_handles(|s| {
        let e = s.run(&js("var e = new RangeError('no'); e.code = 7; e.toString = function () { return 'own ' + this.code; }; throw e")).unwrap_err();
        assert_eq!(e.message(), "RangeError: no");
        let thrown = e.thrown().unwrap();
        let at = s.unrooted(thrown).to_bits();
        s.run(&js(CHURN)).unwrap();
        assert_ne!(s.unrooted(thrown).to_bits(), at, "not moved");
        let code = s.intern("code");
        let c = s.get(thrown, code).unwrap();
        assert_eq!(s.to_number(c).unwrap(), 7.0);
        let class = s.run(&js("RangeError")).unwrap();
        assert!(s.instance_of(thrown, class).unwrap());
        // (Its `toString` runs: the error is no VM's last any more.)
        assert_eq!(s.error_message(&e), "own 7");
        // An operation's error, a primitive thrown, the VM's own.
        let o = s.run(&js("({get x() { throw 'from a getter'; }})")).unwrap();
        let x = s.intern("x");
        let e2 = s.get(o, x).unwrap_err();
        assert_eq!(s.display(e2.thrown().unwrap()), "from a getter");
        let own = s.run(&js("null.x")).unwrap_err();
        assert!(own.thrown().is_none() && own.message().starts_with("TypeError"));
        s.owned_error(e)
    });
    assert_eq!(error.0, "own 7");
    assert!(error.1.is_none());
}

/// Each operation of the scope, once.
#[test]
fn scope_operations() {
    let mut vm = Vm::new();
    vm.with_handles(|s| {
        let o = s.run(&js("({a: 1, b: 2})")).unwrap();
        let (a, c) = (s.intern("a"), s.intern("c"));
        let three = s.root(Local::from(3));
        s.set(o, c, three).unwrap();
        let key = s.string("b").unwrap();
        assert!(s.has_property(o, key).unwrap());
        assert!(s.delete_property(o, key).unwrap());
        assert!(!s.has_property(o, key).unwrap());
        let keys = s.for_in_keys(o).unwrap();
        let join = s.intern("join");
        let f = s.get(keys, join).unwrap();
        let text = s.call(f, keys, &[]).unwrap();
        assert_eq!(s.display(text), "a,c");
        let t = s.type_of(o);
        assert_eq!(s.display(t), "object");
        let v = s.get(o, a).unwrap();
        assert_eq!(s.to_number(v).unwrap(), 1.0);
        let null = s.root(Local::NULL);
        let bare = s.object(null);
        assert!(s.get(bare, a).is_ok());
        let array = s.run(&js("Array")).unwrap();
        let ten = s.root(Local::from(10));
        let made = s.construct(array, &[ten]).unwrap();
        let length = s.intern("length");
        let n = s.get(made, length).unwrap();
        assert_eq!(s.to_number(n).unwrap(), 10.0);
        let max = s.run(&js("Math.max")).unwrap();
        let undefined = s.root(Local::UNDEFINED);
        let args: Vec<_> = (0..20).map(|i| s.root(Local::from(i))).collect();
        let m = s.call(max, undefined, &args).unwrap();
        assert_eq!(s.display(m), "19");
        let g = s.global_object();
        let mathname = s.intern("Math");
        assert!(s.get(g, mathname).is_ok());
        let units: Vec<u16> = "é\u{1F600}".encode_utf16().collect();
        let u = s.string_utf16(&units).unwrap();
        assert_eq!(s.string_units(u).as_deref(), Some(&units[..]));
        assert_eq!(s.string_of(u).as_deref(), Some("é\u{1F600}"));
        assert!(s.string_of(o).is_none());
        let owned = s.run_owned(js("'owned'")).unwrap();
        assert_eq!(s.display(owned), "owned");
    });
}
