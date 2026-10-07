//! The object model and calls, through small bytecode listings: inline
//! caches and their invalidation, constructors, prototypes, arrays,
//! natives, errors and frames.

use toi::value::Value;
use toi::vm::{Error, Vm, display};

/// Run `src` in the interpreter only, then tiering up after 1 and 2 loop
/// back edges (so loops compile mid-run, and re-enter compiled code right
/// after exits), then with the default tiering; the results must agree.
fn run(src: &str) -> Result<Value, Error> {
    let m = toi::asm::parse(src).unwrap_or_else(|e| panic!("{e}"));
    let mut results = Vec::new();
    for budget in [i64::MAX, 1, 2, 1000] {
        let mut vm = Vm::new();
        vm.set_tier_back_edges(budget);
        let r = vm.run(&m);
        results.push(r.as_ref().map(|&v| display(v)).map_err(Clone::clone));
        if budget == 1000 {
            assert!(
                results.windows(2).all(|w| w[0] == w[1]),
                "interpreter, tier at 1, 2, 1000: {results:?}"
            );
            return r;
        }
    }
    unreachable!()
}

fn ok(src: &str) -> String {
    display(run(src).unwrap_or_else(|e| panic!("{e}")))
}

/// `script` calling `main()` and returning its result, plus `rest`.
fn with_main(rest: &str) -> String {
    format!(
        "script\n CreateClosure main\n StaGlobal main\n LdaGlobal main\n Star r1\n \
         CallUndefined r1, r2, 0\n Return\nend\n{rest}"
    )
}

#[test]
fn globals_and_reference_errors() {
    assert_eq!(
        ok(
            "script\n LdaSmi 4\n StaGlobal x\n LdaSmi 5\n StaGlobal x\n LdaGlobal x\n Return\nend\n"
        ),
        "5"
    );
    let e = run("script\n LdaGlobal nope\n Return\nend\n").unwrap_err();
    assert_eq!(e.0, "ReferenceError: nope is not defined");
}

#[test]
fn named_properties_in_and_out_of_line() {
    // Ten properties on an array-literal-free plain object: the first four
    // are in-object (initial capacity), the rest out of line. Each store
    // and load site is run 3 times, so the second and third hit the cache.
    let mut f = String::from("function main()\n LdaSmi 0\n Star r3\nloop:\n");
    f += " LdaGlobal O\n Star r4\n Construct r4, r5, 0\n Star r1\n";
    for i in 0..10 {
        f += &format!(" LdaSmi {i}\n SetNamed r1, p{i}\n");
    }
    f += " LdaSmi 0\n Star r2\n";
    for i in 0..10 {
        f += &format!(" GetNamed r1, p{i}\n Add r2\n Star r2\n");
    }
    f += " Ldar r3\n Inc\n Star r3\n JumpIfLessSmi 3, loop\n Ldar r2\n Return\nend\n";
    f += "function O()\n LdaUndefined\n Return\nend\n";
    let src = with_main(&f).replace("script\n", "script\n CreateClosure O\n StaGlobal O\n");
    assert_eq!(ok(&src), "45");
}

#[test]
fn missing_properties_are_undefined() {
    let src = with_main(
        "function main()\n LdaGlobal F\n Star r2\n Construct r2, r3, 0\n Star r1\n \
         GetNamed r1, nothing\n Return\nend\nfunction F()\n LdaUndefined\n Return\nend\n",
    )
    .replace("script\n", "script\n CreateClosure F\n StaGlobal F\n");
    assert_eq!(ok(&src), "undefined");
}

#[test]
fn reading_a_property_of_undefined_is_a_type_error() {
    let e = run("script\n LdaUndefined\n Star r1\n GetNamed r1, x\n Return\nend\n").unwrap_err();
    assert_eq!(
        e.0,
        "TypeError: Cannot read properties of undefined (reading 'x')"
    );
}

/// `function K() {}`, `K.prototype.m = function () { return 1; }`, then a
/// loop calling `k.m()` that, half way, redefines `m` on the prototype, and
/// later shadows it on `k` itself. The cached prototype load must notice
/// both.
#[test]
fn prototype_loads_see_redefinition_and_shadowing() {
    let src = "\
script
        CreateClosure K
        StaGlobal K
        LdaGlobal K
        Star r1
        GetNamed r1, prototype
        Star r1
        CreateClosure one
        SetNamed r1, m
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function K()
        LdaUndefined
        Return
end
function one()
        LdaSmi 1
        Return
end
function ten()
        LdaSmi 10
        Return
end
function hundred()
        LdaSmi 100
        Return
end
; r1 k, r2 i, r3 sum, r4 callee, r5 this, r6 temp
function main()
        LdaGlobal K
        Star r4
        Construct r4, r5, 0
        Star r1
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, m
        Star r4
        Mov r1, r5
        Call r4, r5, 0
        Add r3
        Star r3
        Ldar r2
        TestEqualSmi 4
        JumpIfFalse not4
        LdaGlobal K             ; K.prototype.m = ten (a value change)
        Star r6
        GetNamed r6, prototype
        Star r6
        CreateClosure ten
        SetNamed r6, m
not4:   Ldar r2
        TestEqualSmi 7
        JumpIfFalse not7
        CreateClosure hundred   ; k.m = hundred (shadows: k's shape changes)
        SetNamed r1, m
not7:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
        Ldar r3
        Return
end
";
    // i = 0..4 → 1 (5 calls), 5..7 → 10 (3 calls), 8..9 → 100 (2 calls).
    assert_eq!(ok(src), "235");
}

/// A property added to a prototype after a miss was cached must be found:
/// `k.late` is undefined, then `K.prototype.late = 7`, then `k.late` again
/// at the same site.
#[test]
fn adding_to_a_prototype_invalidates_cached_loads() {
    let src = "\
script
        CreateClosure K
        StaGlobal K
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function K()
        LdaUndefined
        Return
end
; r1 k, r2 i, r3 count of defined reads, r4-r5 temps
function main()
        LdaGlobal K
        Star r4
        Construct r4, r5, 0
        Star r1
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, constructor ; a prototype load, cached
        Star r4
        LdaGlobal K
        TestEqual r4
        JumpIfFalse bad
        GetNamed r1, late
        JumpIfToBooleanFalse skip
        Ldar r3
        Inc
        Star r3
skip:   Ldar r2
        TestEqualSmi 2
        JumpIfFalse next
        LdaGlobal K
        Star r4
        GetNamed r4, prototype
        Star r4
        LdaSmi 7
        SetNamed r4, late
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 6, loop
        Ldar r3
        Return
bad:    LdaSmi -1
        Return
end
";
    assert_eq!(ok(src), "3");
}

#[test]
fn constructor_returning_an_object_replaces_this() {
    let src = with_main(
        "function main()\n LdaGlobal F\n Star r1\n Construct r1, r2, 0\n Star r1\n GetNamed r1, v\n Return\nend\n\
         function G()\n LdaSmi 9\n SetNamed r0, v\n LdaUndefined\n Return\nend\n\
         function F()\n LdaSmi 1\n SetNamed r0, v\n LdaGlobal G\n Star r1\n Construct r1, r2, 0\n Return\nend\n",
    )
    .replace("script\n", "script\n CreateClosure F\n StaGlobal F\n CreateClosure G\n StaGlobal G\n");
    assert_eq!(ok(&src), "9");
}

#[test]
fn missing_arguments_are_undefined_and_extra_ones_ignored() {
    let src = with_main(
        "function main()\n LdaGlobal f\n Star r1\n LdaSmi 5\n Star r2\n Star r3\n Star r4\n \
         CallUndefined r1, r2, 1\n Star r5\n CallUndefined r1, r2, 3\n Add r5\n Return\nend\n\
         function f(a, b)\n Ldar r2\n JumpIfToBooleanTrue has\n LdaSmi 100\n Return\n\
         has: Ldar r1\n Add r2\n Return\nend\n",
    )
    .replace("script\n", "script\n CreateClosure f\n StaGlobal f\n");
    // f(5) → b undefined → 100; f(5, 5, 5) → 10.
    assert_eq!(ok(&src), "110");
}

#[test]
fn recursion_uses_the_vm_stack() {
    // fib(20) = 6765, about 22k calls.
    let src = with_main(
        "function main()\n LdaGlobal fib\n Star r1\n LdaSmi 20\n Star r2\n CallUndefined r1, r2, 1\n Return\nend\n\
         function fib(n)\n Ldar r1\n JumpIfLessSmi 2, base\n LdaGlobal fib\n Star r2\n Ldar r1\n AddSmi -1\n Star r3\n \
         CallUndefined r2, r3, 1\n Star r4\n Ldar r1\n AddSmi -2\n Star r3\n CallUndefined r2, r3, 1\n Add r4\n Return\n\
         base: Ldar r1\n Return\nend\n",
    )
    .replace("script\n", "script\n CreateClosure fib\n StaGlobal fib\n");
    assert_eq!(ok(&src), "6765");
}

#[test]
fn unbounded_recursion_is_a_range_error() {
    // `main() + 1`: not a tail call, which compiled code would make a
    // loop (unbounded, that runs forever).
    let src = with_main(
        "function main()\n LdaGlobal main\n Star r1\n CallUndefined r1, r2, 0\n AddSmi 1\n Return\nend\n",
    );
    let e = run(&src).unwrap_err();
    assert_eq!(e.0, "RangeError: Maximum call stack size exceeded");
}

#[test]
fn calling_a_non_function_is_a_type_error() {
    let e =
        run("script\n LdaSmi 3\n Star r1\n CallUndefined r1, r2, 0\n Return\nend\n").unwrap_err();
    assert!(e.0.starts_with("TypeError: 3 is not a function"), "{e}");
    let e = run("script\n LdaNull\n Star r1\n Construct r1, r2, 0\n Return\nend\n").unwrap_err();
    assert!(
        e.0.starts_with("TypeError: null is not a constructor"),
        "{e}"
    );
}

#[test]
fn arrays_push_index_holes_and_length() {
    let src = "\
script                          ; r1 a, r2 push, r3 this, r4 arg, r5 key
        CreateArray
        Star r1
        GetNamed r1, push
        Star r2
        Mov r1, r3
        LdaSmi 10
        Star r4
        Call r2, r3, 1          ; a.push(10) → 1
        LdaSmi 5
        Star r5
        LdaSmi 50
        SetKeyed r1, r5         ; a[5] = 50: holes at 1..4
        LdaSmi 3
        GetKeyed r1             ; a[3] → undefined
        Star r6
        LdaUndefined
        TestEqual r6
        JumpIfFalse bad
        LdaSmi 0
        GetKeyed r1
        Star r6                 ; a[0] = 10
        LdaSmi 5
        GetKeyed r1
        Add r6
        Star r6                 ; + a[5] = 60
        GetNamed r1, length
        Add r6                  ; + 6
        Return
bad:    LdaSmi -1
        Return
end
";
    assert_eq!(ok(src), "66");
}

#[test]
fn loose_equality_and_truthiness() {
    let cases = [
        ("LdaNull", "LdaUndefined", "TestEqual", true),
        ("LdaNull", "LdaSmi 0", "TestEqual", false),
        ("LdaTrue", "LdaSmi 1", "TestEqual", true),
        ("LdaFalse", "LdaSmi 0", "TestEqual", true),
        ("LdaSmi 2", "LdaSmi 2", "TestEqual", true),
    ];
    for (a, b, op, want) in cases {
        let src = format!("script\n {a}\n Star r1\n {b}\n {op} r1\n Return\nend\n");
        assert_eq!(ok(&src), want.to_string(), "{a} == {b}");
    }
    for (v, truthy) in [
        ("LdaSmi 0", false),
        ("LdaSmi -3", true),
        ("LdaNull", false),
        ("CreateArray", true),
    ] {
        let src = format!(
            "script\n {v}\n JumpIfToBooleanTrue t\n LdaFalse\n Return\nt: LdaTrue\n Return\nend\n"
        );
        assert_eq!(ok(&src), truthy.to_string(), "{v}");
    }
}

#[test]
fn modulo_and_bitwise_edge_cases() {
    let one = |body: &str| ok(&format!("script\n{body}\n Return\nend\n"));
    assert_eq!(one(" LdaSmi -7\n ModSmi 3"), "-1");
    assert_eq!(
        display(run("script\n LdaSmi -6\n ModSmi 3\n Return\nend\n").unwrap()),
        "0"
    );
    let v = run("script\n LdaSmi -6\n ModSmi 3\n Return\nend\n").unwrap();
    assert!(
        v.is_double() && v.as_f64_unchecked().is_sign_negative(),
        "-6 % 3 is -0"
    );
    assert_eq!(one(" LdaSmi 7\n ModSmi 0"), "NaN");
    assert_eq!(
        one(" LdaSmi 2147483647\n AddSmi 1\n BitOrSmi 0"),
        "-2147483648"
    );
    assert_eq!(one(" LdaUndefined\n BitAndSmi 7"), "0");
    assert_eq!(one(" LdaSmi 13\n BitAndSmi 6"), "4");
}

#[test]
fn slack_tracking_grows_in_object_capacity() {
    // A constructor setting 6 fields: the first object overflows the
    // initial 4 in-object slots; later ones get 6.
    let mut ctor = String::from("function P()\n");
    for i in 0..6 {
        ctor += &format!(" LdaSmi {i}\n SetNamed r0, f{i}\n");
    }
    ctor += " LdaUndefined\n Return\nend\n";
    let src = format!(
        "script\n CreateClosure P\n StaGlobal P\n LdaGlobal P\n Star r1\n Construct r1, r2, 0\n StaGlobal first\n \
         Construct r1, r2, 0\n Return\nend\n{ctor}"
    );
    let m = toi::asm::parse(&src).unwrap();
    let mut vm = Vm::new();
    let second = vm.run(&m).unwrap();
    assert_eq!(vm.inline_capacity(second), 6);
}

/// `k → B.prototype → A.prototype.m`: the load of `k.m` is cached with
/// `A.prototype` as holder. Adding `m` to `B.prototype` changes no shape
/// the cache checks, so only the prototype epoch catches it.
#[test]
fn shadowing_on_an_intermediate_prototype_invalidates_cached_loads() {
    let src = "\
script                          ; r1 temp, r3 new A()
        CreateClosure A
        StaGlobal A
        CreateClosure B
        StaGlobal B
        LdaGlobal A
        Star r1
        GetNamed r1, prototype
        Star r1
        CreateClosure one
        SetNamed r1, m          ; A.prototype.m = one
        LdaGlobal A
        Star r1
        Construct r1, r2, 0
        Star r3
        LdaGlobal B
        Star r1
        Ldar r3
        SetNamed r1, prototype  ; B.prototype = new A()
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function A()
        LdaUndefined
        Return
end
function B()
        LdaUndefined
        Return
end
function one()
        LdaSmi 1
        Return
end
function ten()
        LdaSmi 10
        Return
end
; r1 k, r2 i, r3 sum, r4 callee, r5 this, r6 temp
function main()
        LdaGlobal B
        Star r4
        Construct r4, r5, 0
        Star r1
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, m
        Star r4
        Mov r1, r5
        Call r4, r5, 0
        Add r3
        Star r3
        Ldar r2
        TestEqualSmi 2
        JumpIfFalse next
        LdaGlobal B
        Star r6
        GetNamed r6, prototype
        Star r6
        CreateClosure ten
        SetNamed r6, m          ; B.prototype.m = ten
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 6, loop
        Ldar r3
        Return
end
";
    assert_eq!(ok(src), "33");
}

// ---- Property accesses in compiled loops ----
//
// These loops make no calls, so the JIT compiles them (`run` also tiers up
// after 2 back edges, mid-loop). `jit` runs that configuration alone and
// returns the stencils of `main`'s last compiled code.

fn jit(src: &str) -> (String, Vec<String>) {
    let expected = ok(src);
    let m = toi::asm::parse(src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(2);
    let v = display(vm.run(&m).unwrap());
    assert_eq!(v, expected);
    (v, vm.jit_stencils("main").unwrap_or_default())
}

/// `k → B.prototype → A.prototype.v = 1`, read in a loop; at i = 4,
/// `B.prototype.v = 10` shadows it. The compiled load reads A.prototype's
/// slot directly, so only the prototype epoch, checked when entering
/// compiled code, catches the change.
#[test]
fn compiled_prototype_loads_see_shadowing() {
    let src = "\
script                          ; r1 temp, r3 new A()
        CreateClosure A
        StaGlobal A
        CreateClosure B
        StaGlobal B
        LdaGlobal A
        Star r1
        GetNamed r1, prototype
        Star r1
        LdaSmi 1
        SetNamed r1, v          ; A.prototype.v = 1
        LdaGlobal A
        Star r1
        Construct r1, r2, 0
        Star r3
        LdaGlobal B
        Star r1
        Ldar r3
        SetNamed r1, prototype  ; B.prototype = new A()
        LdaGlobal B
        Star r1
        Construct r1, r2, 0
        StaGlobal k             ; k = new B()
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function A()
        LdaUndefined
        Return
end
function B()
        LdaUndefined
        Return
end
; r1 k, r2 i, r3 sum, r4 B.prototype
function main()
        LdaGlobal k
        Star r1
        LdaGlobal B
        Star r4
        GetNamed r4, prototype
        Star r4
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, v
        Add r3
        Star r3
        Ldar r2
        TestEqualSmi 4
        JumpIfFalse next
        LdaSmi 10
        SetNamed r4, v          ; B.prototype.v = 10
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 20, loop
        Ldar r3
        Return
end
";
    // i = 0..4 read 1, i = 5..19 read 10.
    let (v, s) = jit(src);
    assert_eq!(v, "155");
    assert!(s.iter().any(|n| n.starts_with("get_proto")), "{s:?}");
}

/// Objects of two shapes at one site: guards fail, the interpreter's
/// cache flips, and after a few exits the access is left to the
/// interpreter. The loop stays correct throughout.
#[test]
fn polymorphic_sites_settle_in_the_interpreter() {
    let src = "\
script
        CreateClosure P
        StaGlobal P
        CreateClosure Q
        StaGlobal Q
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function P(x)                   ; x first
        Ldar r1
        SetNamed r0, x
        LdaUndefined
        Return
end
function Q(x)                   ; another field first, so x is at another offset
        LdaSmi 0
        SetNamed r0, y
        Ldar r1
        SetNamed r0, x
        LdaUndefined
        Return
end
; r1 array, r2 i, r3 sum, r4-r7 temps
function main()
        CreateArray
        Star r1
        LdaSmi 0
        Star r2
fill:   GetNamed r1, push
        Star r4
        Mov r1, r5
        LdaGlobal P
        Star r7
        Construct r7, r2, 1
        Star r6
        Call r4, r5, 1
        GetNamed r1, push
        Star r4
        Mov r1, r5
        LdaGlobal Q
        Star r7
        Construct r7, r2, 1
        Star r6
        Call r4, r5, 1
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 50, fill
        LdaSmi 0
        Star r2
        Star r3
loop:   Ldar r2
        GetKeyed r1
        Star r4
        GetNamed r4, x
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 100, loop
        Ldar r3
        Return
end
";
    // Elements are P(0), Q(0), P(1), Q(1), …: twice 0 + … + 49.
    assert_eq!(jit(src).0, "2450");
}

/// A loop adding a field to objects built without it: once the store's
/// cache holds the transition, the compiled store moves each object to the
/// new shape itself (`set_tr`). Out-of-bounds reads then exit.
#[test]
fn compiled_stores_transition_shapes() {
    let src = "\
script
        CreateClosure E
        StaGlobal E
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function E()
        LdaUndefined
        Return
end
; r1 array, r2 i, r3 sum, r4-r7 temps
function main()
        CreateArray
        Star r1
        LdaSmi 0
        Star r2
fill:   GetNamed r1, push
        Star r4
        Mov r1, r5
        LdaGlobal E
        Star r7
        Construct r7, r8, 0
        Star r6
        Call r4, r5, 1
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 40, fill
        LdaSmi 0
        Star r2
        Star r3
loop:   Ldar r2
        GetKeyed r1
        Star r4
        Ldar r2
        SetNamed r4, x          ; o.x = i: a transition, the first time
        GetNamed r4, x
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 40, loop
        Ldar r3
        Return
end
";
    let (v, s) = jit(src);
    assert_eq!(v, "780");
    assert!(s.iter().any(|n| n.starts_with("set_tr_")), "{s:?}");
    // After the transition the receiver's shape is known: the load is free.
    assert!(s.iter().any(|n| n.starts_with("get_own_nc_")), "{s:?}");
}

#[test]
fn compiled_globals() {
    // A global counter incremented in a loop: `lda_global` / `sta_global`.
    let src = "\
script
        LdaSmi 0
        StaGlobal count
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function main()                 ; r1 i
        LdaSmi 0
        Star r1
loop:   LdaGlobal count
        AddSmi 3
        StaGlobal count
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 1000, loop
        LdaGlobal count
        Return
end
";
    let (v, s) = jit(src);
    assert_eq!(v, "3000");
    assert!(s.iter().any(|n| n.starts_with("lda_global")), "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("sta_global")), "{s:?}");
}

#[test]
fn compiled_keyed_loads_exit_on_holes_and_bounds() {
    // a = [10, 11, <hole>, 13]; the loop reads a[0..6]: a hole and two
    // out-of-bounds indices read undefined (counted as 100 each).
    let src = "\
script
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
; r1 a, r2 i, r3 sum, r4 key, r5 temp
function main()
        CreateArray
        Star r1
        LdaSmi 0
        Star r4
        LdaSmi 10
        SetKeyed r1, r4
        LdaSmi 1
        Star r4
        LdaSmi 11
        SetKeyed r1, r4
        LdaSmi 3
        Star r4
        LdaSmi 13
        SetKeyed r1, r4
        LdaSmi 0
        Star r2
        Star r3
loop:   Ldar r2
        GetKeyed r1
        Star r5
        LdaUndefined
        TestEqual r5
        JumpIfFalse value
        LdaSmi 100
        Star r5
value:  Ldar r5
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 6, loop
        Ldar r3
        Return
end
";
    assert_eq!(jit(src).0, "334");
}

// ---- Field types ----

/// A loop summing `o.x` over an object whose `x` is an int32, compiled with
/// `x` typed int32; at i = 10, `o.x = -0` (a double: `-2 % 2`, kept in a
/// global) makes the field a number. The interpreter generalizes it, and
/// code compiled for the old type is dropped.
#[test]
fn a_field_generalizing_drops_compiled_code() {
    let src = "\
script
        LdaSmi -2
        ModSmi 2
        StaGlobal nzero
        CreateClosure P
        StaGlobal P
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function P(x)
        Ldar r1
        SetNamed r0, x
        LdaUndefined
        Return
end
; r1 o, r2 i, r3 sum, r4-r5 temps
function main()
        LdaGlobal P
        Star r4
        LdaSmi 1
        Star r5
        Construct r4, r5, 1
        Star r1                 ; o = new P(1)
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, x
        Add r3
        Star r3                 ; sum += o.x
        Ldar r2
        TestEqualSmi 10
        JumpIfFalse next
        LdaGlobal nzero
        SetNamed r1, x          ; o.x = -0
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 20, loop
        Ldar r3
        Return
end
";
    // 11 × 1, then 9 × -0.
    let (v, s) = jit(src);
    assert_eq!(v, "11");
    // Recompiled with `x` a number: loaded into `facc`.
    assert!(s.iter().any(|n| n.ends_with("+unbox_num_acc")), "{s:?}");
}

/// `box.v` holds objects of shape A, so `box.v.n` is loaded without a
/// guard; at i = 10 the compiled loop stores a B there (`n` at another
/// offset). The store's check of the value against the field's type must
/// exit, so the interpreter generalizes the field before the store.
#[test]
fn object_typed_fields_generalize_on_another_shape() {
    let src = "\
script
        CreateClosure Box
        StaGlobal Box
        CreateClosure A
        StaGlobal A
        CreateClosure B
        StaGlobal B
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function Box(v)
        Ldar r1
        SetNamed r0, v
        LdaUndefined
        Return
end
function A()                    ; { n: 1 }
        LdaSmi 1
        SetNamed r0, n
        LdaUndefined
        Return
end
function B()                    ; { pad: 0, n: 100 }: n at another offset
        LdaSmi 0
        SetNamed r0, pad
        LdaSmi 100
        SetNamed r0, n
        LdaUndefined
        Return
end
; r1 box, r2 i, r3 sum, r4-r8 temps, r9 [new A(), new B()]
function main()
        CreateArray
        Star r9
        GetNamed r9, push
        Star r4
        Mov r9, r5
        LdaGlobal A
        Star r7
        Construct r7, r8, 0
        Star r6
        Call r4, r5, 1
        LdaGlobal B
        Star r7
        Construct r7, r8, 0
        Star r6
        Call r4, r5, 1
        LdaGlobal Box
        Star r4
        LdaSmi 0
        GetKeyed r9
        Star r5
        Construct r4, r5, 1
        Star r1                 ; box = new Box(a[0])
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, v
        Star r4
        GetNamed r4, n
        Add r3
        Star r3                 ; sum += box.v.n
        Ldar r2
        TestEqualSmi 10
        JumpIfFalse next
        LdaSmi 1
        GetKeyed r9
        SetNamed r1, v          ; box.v = a[1], a B
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 20, loop
        Ldar r3
        Return
end
";
    // 11 × 1, then 9 × 100.
    let (v, s) = jit(src);
    assert_eq!(v, "911");
    let _ = s;
}

/// Field types are assumed by compiled code even where no inline cache
/// says so: here `o.y` is first loaded at i = 15, in compiled code, typed
/// from `o`'s shape, int32. At i = 5 another function (called, so in the
/// interpreter) stores a double into `o.y`. Nothing in `main`'s caches
/// changes; only the field epoch drops the code assuming int32.
#[test]
fn field_types_changed_elsewhere_drop_compiled_code() {
    let src = "\
script
        LdaSmi 2147483647
        AddSmi 3
        StaGlobal big           ; 2147483650, a double
        CreateClosure P
        StaGlobal P
        CreateClosure g
        StaGlobal g
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function P(y)
        Ldar r1
        SetNamed r0, y
        LdaUndefined
        Return
end
function g()                    ; O.y = big
        LdaGlobal O
        Star r1
        LdaGlobal big
        SetNamed r1, y
        LdaUndefined
        Return
end
; r1 o, r2 i, r3 sum, r4-r5 temps, r6 g
function main()
        LdaGlobal P
        Star r4
        LdaSmi 1
        Star r5
        Construct r4, r5, 1
        Star r1
        StaGlobal O             ; O = o = new P(1)
        LdaGlobal g
        Star r6                 ; loaded before the loop: no cache of main's
        LdaSmi 0                ; changes when g runs
        Star r2
        Star r3
loop:   Ldar r2
        TestEqualSmi 5
        JumpIfFalse late
        CallUndefined r6, r5, 0 ; g()
late:   Ldar r2
        JumpIfLessSmi 15, next
        GetNamed r1, y
        Add r3
        Star r3                 ; sum += o.y, for i >= 15
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 20, loop
        Ldar r3
        Return
end
";
    assert_eq!(jit(src).0, "10737418250");
}

/// An interpreter store whose cache hits must still check the value
/// against the field's type: `set(o, 1)` caches the store with `y` an
/// int32, then `set(o, big)` hits that cache with a double. The field must
/// generalize there, or the compiled loop after it would read `o.y` as an
/// int32.
#[test]
fn cached_interpreter_stores_check_field_types() {
    let src = "\
script
        LdaSmi 2147483647
        AddSmi 3
        StaGlobal big           ; 2147483650, a double
        CreateClosure P
        StaGlobal P
        CreateClosure set
        StaGlobal set
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function P(y)
        Ldar r1
        SetNamed r0, y
        LdaUndefined
        Return
end
function set(o, v)              ; o.y = v
        Ldar r2
        SetNamed r1, y
        LdaUndefined
        Return
end
; r1 o, r2 i, r3 sum, r4-r6 temps
function main()
        LdaGlobal P
        Star r4
        LdaSmi 1
        Star r5
        Construct r4, r5, 1
        Star r1                 ; o = new P(1)
        LdaGlobal set
        Star r4
        Mov r1, r5
        LdaSmi 1
        Star r6
        CallUndefined r4, r5, 2 ; set(o, 1)
        LdaGlobal big
        Star r6
        CallUndefined r4, r5, 2 ; set(o, big)
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, y
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
        Ldar r3
        Return
end
";
    assert_eq!(jit(src).0, "21474836500");
}

// ---- Element types ----

/// `a = [1, 2, 3, 4]`, summed in a loop as `a[i & 3]`, compiled with the
/// elements typed int32 (from `a`'s shape, known at entry). At i = 10,
/// `a[2] = big` (a double) moves `a` to a shape with number elements; the
/// code compiled for the old shape is no longer entered.
#[test]
fn element_types_generalize_by_shape() {
    let src = "\
script
        LdaSmi 2147483647
        AddSmi 3
        StaGlobal big           ; 2147483650, a double
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
; r1 a, r2 i, r3 sum, r4 key
function main()
        CreateArray
        Star r1
        LdaSmi 0
        Star r4
        LdaSmi 1
        SetKeyed r1, r4
        LdaSmi 1
        Star r4
        LdaSmi 2
        SetKeyed r1, r4
        LdaSmi 2
        Star r4
        LdaSmi 3
        SetKeyed r1, r4
        LdaSmi 3
        Star r4
        LdaSmi 4
        SetKeyed r1, r4         ; a = [1, 2, 3, 4]
        LdaSmi 0
        Star r2
        Star r3
loop:   Ldar r2
        BitAndSmi 3
        GetKeyed r1
        Add r3
        Star r3                 ; sum += a[i & 3]
        Ldar r2
        TestEqualSmi 10
        JumpIfFalse next
        LdaSmi 2
        Star r4
        LdaGlobal big
        SetKeyed r1, r4         ; a[2] = big
next:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 20, loop
        Ldar r3
        Return
end
";
    // i = 0..10: 1+2+3+4 twice, then 1+2+3 (26); i = 11..19, with a =
    // [1, 2, big, 4]: 4, 1, 2, big, 4, 1, 2, big, 4 (18 + 2 big).
    let (v, s) = jit(src);
    assert_eq!(v, (26 + 18 + 2 * 2147483650u64).to_string());
    // Recompiled for number elements: loaded into `facc`.
    assert!(s.iter().any(|n| n.ends_with("+unbox_num_acc")), "{s:?}");
}

/// An array reached through a field (`box.list`) is only `Ext` in compiled
/// code, which says nothing about its elements; the keyed load's cache
/// supplies its shape, which a guard checks, and the elements' type with
/// it: `box.list[i].n` needs no guard on the element.
#[test]
fn keyed_loads_guard_the_cached_shape_for_element_types() {
    let src = "\
script
        CreateClosure Box
        StaGlobal Box
        CreateClosure N
        StaGlobal N
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function Box()
        CreateArray
        SetNamed r0, list
        LdaUndefined
        Return
end
function N(n)
        Ldar r1
        SetNamed r0, n
        LdaUndefined
        Return
end
; r1 box, r2 i, r3 sum, r4-r7 temps
function main()
        LdaGlobal Box
        Star r4
        Construct r4, r5, 0
        Star r1
        LdaSmi 0
        Star r2
fill:   GetNamed r1, list
        Star r5
        GetNamed r5, push
        Star r4
        LdaGlobal N
        Star r7
        Construct r7, r2, 1
        Star r6
        Call r4, r5, 1          ; box.list.push(new N(i))
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, fill
        LdaSmi 0
        Star r2
        Star r3
loop:   GetNamed r1, list
        Star r4
        Ldar r2
        GetKeyed r4
        Star r4
        GetNamed r4, n
        Add r3
        Star r3                 ; sum += box.list[i].n
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
        Ldar r3
        Return
end
";
    let (v, s) = jit(src);
    assert_eq!(v, "45");
    assert!(s.iter().any(|n| n.starts_with("get_keyed_i_sh_")), "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("get_own_nc_")), "{s:?}");
}

// ---- Calls in compiled loops ----

/// `get(o)` inlined into a loop over objects of two shapes: the inlined
/// load's guard fails on the second shape, inside the inlined frame, so the
/// exit writes the callee's frame header and the interpreter finishes the
/// call there, returning into the caller. An int32 overflow inside the
/// inlined callee exits the same way.
#[test]
fn exits_inside_inlined_callees_resume_in_the_callee() {
    let src = "\
script
        LdaSmi 0
        StaGlobal calls
        CreateClosure P
        StaGlobal P
        CreateClosure Q
        StaGlobal Q
        CreateClosure get
        StaGlobal get
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function P(x)
        Ldar r1
        SetNamed r0, x
        LdaUndefined
        Return
end
function Q(x)                   ; x at another offset
        LdaSmi 0
        SetNamed r0, pad
        Ldar r1
        SetNamed r0, x
        LdaUndefined
        Return
end
function get(o)                 ; calls++; o.x + 2147483000: overflows for large x
        LdaGlobal calls
        Inc
        StaGlobal calls
        GetNamed r1, x
        AddSmi 2147483000
        Return
end
; r1 p, r2 q, r3 i, r4 sum (a double eventually), r5 get, r6 arg, r7 temp
function main()
        LdaGlobal P
        Star r7
        LdaSmi 1
        Star r6
        Construct r7, r6, 1
        Star r1
        LdaGlobal Q
        Star r7
        LdaSmi 1000
        Star r6
        Construct r7, r6, 1
        Star r2
        LdaGlobal get
        Star r5
        LdaSmi 0
        Star r3
        Star r4
loop:   Mov r1, r6
        Ldar r3
        TestEqualSmi 10
        JumpIfFalse call
        Mov r2, r6              ; at i = 10, get(q)
call:   CallUndefined r5, r6, 1
        Add r4
        Star r4
        Ldar r3
        Inc
        Star r3
        JumpIfLessSmi 20, loop
        LdaGlobal calls
        Add r4                  ; + the number of calls, 20: an exit
        Return                  ; inside `get` must not call it again
end
";
    // 19 × (1 + 2147483000) + (1000 + 2147483000) + 20.
    let (v, s) = jit(src);
    assert_eq!(v, (19 * 2147483001u64 + 2147484000 + 20).to_string());
    assert!(s.iter().any(|n| n.starts_with("check_callee_")), "{s:?}");
}

/// The function a call site calls changes mid-loop (a global reassigned):
/// the inlined call's guard exits, and the interpreter makes the call.
#[test]
fn inlined_calls_guard_the_callee() {
    let src = "\
script
        CreateClosure one
        StaGlobal f
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function one()
        LdaSmi 1
        Return
end
function ten()
        LdaSmi 10
        Return
end
; r1 i, r2 sum, r3 callee
function main()
        LdaSmi 0
        Star r1
        Star r2
loop:   Ldar r1
        TestEqualSmi 10
        JumpIfFalse call
        CreateClosure ten
        StaGlobal f             ; f = ten, at i = 10
call:   LdaGlobal f
        Star r3
        CallUndefined r3, r4, 0
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 20, loop
        Ldar r2
        Return
end
";
    // 10 × 1 + 10 × 10.
    assert_eq!(jit(src).0, "110");
}

#[test]
fn errors_in_callees_end_compiled_loops() {
    // At i = 5 the callee reads a property of undefined.
    let src = "\
script
        CreateClosure g
        StaGlobal g
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function g(o)
        GetNamed r1, x
        Return
end
; r1 i, r2 g, r3 arg, r4 o
function main()
        LdaGlobal g
        Star r2
        CreateArray
        Star r4
        LdaSmi 0
        Star r1
loop:   Mov r4, r3
        Ldar r1
        TestEqualSmi 5
        JumpIfFalse call
        LdaUndefined
        Star r3
call:   CallUndefined r2, r3, 1
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 20, loop
        Ldar r1
        Return
end
";
    let e = run(src).unwrap_err();
    assert_eq!(
        e.0,
        "TypeError: Cannot read properties of undefined (reading 'x')"
    );
}

#[test]
fn compiled_loops_call_recursive_functions_constructors_and_natives() {
    // sum of fib(i % 15) for i < 30, each pushed onto an array as a new
    // Box(fib); the result adds the array's length.
    let src = "\
script
        CreateClosure fib
        StaGlobal fib
        CreateClosure Box
        StaGlobal Box
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function fib(n)
        Ldar r1
        JumpIfLessSmi 2, base
        LdaGlobal fib
        Star r2
        Ldar r1
        AddSmi -1
        Star r3
        CallUndefined r2, r3, 1
        Star r4
        Ldar r1
        AddSmi -2
        Star r3
        CallUndefined r2, r3, 1
        Add r4
        Return
base:   Ldar r1
        Return
end
function Box(v)
        Ldar r1
        SetNamed r0, v
        LdaUndefined
        Return
end
; r1 i, r2 sum, r3 a, r4-r8 temps
function main()
        CreateArray
        Star r3
        LdaSmi 0
        Star r1
        Star r2
loop:   LdaGlobal fib
        Star r4
        Ldar r1
        ModSmi 15
        Star r5
        CallUndefined r4, r5, 1 ; fib(i % 15)
        Star r5
        LdaGlobal Box
        Star r4
        Construct r4, r5, 1     ; new Box(…)
        Star r8
        GetNamed r8, v
        Add r2
        Star r2
        GetNamed r3, push
        Star r6
        Mov r3, r7
        Call r6, r7, 1          ; a.push(box)
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 30, loop
        GetNamed r3, length
        Add r2
        Return
end
";
    // Twice fib(0) + … + fib(14) = 2 × 986, plus 30.
    let (v, s) = jit(src);
    assert_eq!(v, "2002");
    // With their results predicted: `new Box` (called directly) and `push`
    // (through the runtime).
    assert!(
        s.iter().any(|n| n == "construct_direct+check_acc_shape"),
        "{s:?}"
    );
    assert!(s.iter().any(|n| n == "call+check_acc_int"), "{s:?}");
}

/// A callee (through the runtime: its call site is first reached after
/// compiling) gives `o` its own `m`, shadowing the prototype's. The loop
/// knew `o`'s exact shape; after the call that is only a lower bound, so
/// the prototype load is guarded again and sees the change.
#[test]
fn calls_downgrade_exact_shapes() {
    let src = "\
script
        CreateClosure P
        StaGlobal P
        LdaGlobal P
        Star r1
        GetNamed r1, prototype
        Star r1
        LdaSmi 1
        SetNamed r1, m          ; P.prototype.m = 1
        CreateClosure shadow
        StaGlobal shadow
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function P()
        LdaUndefined
        Return
end
function shadow(o)              ; o.m = 100
        LdaSmi 100
        SetNamed r1, m
        LdaUndefined
        Return
end
; r1 o, r2 i, r3 sum, r4 temp, r5 arg, r6 shadow
function main()
        LdaGlobal P
        Star r4
        Construct r4, r5, 0
        Star r1
        LdaGlobal shadow
        Star r6
        LdaSmi 0
        Star r2
        Star r3
loop:   Ldar r2
        TestEqualSmi 5
        JumpIfFalse read
        Mov r1, r5
        CallUndefined r6, r5, 1 ; shadow(o), at i = 5
read:   GetNamed r1, m
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
        Ldar r3
        Return
end
";
    // i = 0..4: 1; i = 5..9: 100.
    assert_eq!(jit(src).0, "505");
}

// ---- Functions entered compiled ----
//
// A function with a loop, once hot (10 calls; 1 or 2 in `run`'s tiered
// configurations), runs compiled from its first instruction at each call.
// The callers below allocate in their loops (`CreateArray`, which compiled
// code leaves to the interpreter), so they stay interpreted and the calls
// come from the interpreter's call handlers.

/// `add(k)` sums `k + 0..9`; it overflows int32 when `k` is near the top,
/// exiting inside the entered function, which the interpreter finishes.
#[test]
fn functions_entered_compiled_from_the_interpreter() {
    let src = "\
script
        CreateClosure add
        StaGlobal add
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function add(k)                 ; r1 k, r2 i, r3 s
        Ldar r1
        Star r3
        LdaSmi 0
        Star r2
loop:   Ldar r2
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
        Ldar r3
        Return
end
; r1 i, r2 total, r3 add, r4 arg, r5 temp
function main()
        LdaGlobal add
        Star r3
        LdaSmi 0
        Star r1
        Star r2
top:    CreateArray
        Star r5
        Ldar r1
        Star r4
        Ldar r1
        TestEqualSmi 40
        JumpIfFalse call
        LdaSmi 2147483600
        Star r4                 ; add(2147483600): overflows
call:   CallUndefined r3, r4, 1
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 50, top
        Ldar r2
        Return
end
";
    let m = toi::asm::parse(src).unwrap();
    let mut vm = Vm::new();
    let v = display(vm.run(&m).unwrap());
    // Σ (i + 45) for i < 50 except 40, plus 2147483600 + 45.
    let expected: u64 = (0..50u64).filter(|&i| i != 40).map(|i| i + 45).sum::<u64>() + 2147483645;
    assert_eq!(v, expected.to_string());
    assert_eq!(ok(src), v);
    let s = vm.jit_stencils("add").expect("add compiled");
    assert!(s.iter().any(|n| n == "return"), "{s:?}");
}

/// A constructor with a loop, entered compiled through `new`: the result is
/// `this` (the function returns undefined).
#[test]
fn constructors_entered_compiled() {
    let src = "\
script
        CreateClosure Sum
        StaGlobal Sum
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function Sum(n)                 ; this.v = 0 + 1 + … + (n - 1)
        LdaSmi 0
        Star r2
        Star r3
loop:   Ldar r2
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLess r1, loop     ; i < n
        Ldar r3
        SetNamed r0, v
        LdaUndefined
        Return
end
; r1 i, r2 total, r3 Sum, r4 arg, r5 temp
function main()
        LdaGlobal Sum
        Star r3
        LdaSmi 0
        Star r1
        Star r2
top:    CreateArray
        Star r5
        Ldar r1
        BitAndSmi 7
        AddSmi 1
        Star r4
        Construct r3, r4, 1     ; new Sum((i & 7) + 1)
        Star r5
        GetNamed r5, v
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 40, top
        Ldar r2
        Return
end
";
    // n = 1..8, five times each: 5 × Σ n(n-1)/2 = 5 × 84.
    assert_eq!(ok(src), "420");
    let mut vm = Vm::new();
    vm.run(&toi::asm::parse(src).unwrap()).unwrap();
    assert!(vm.jit_stencils("Sum").is_some(), "Sum compiled");
}

/// `depth(n)` loops over nothing much, then recurses: compiled, its
/// recursive call goes through the runtime and enters itself compiled.
#[test]
fn recursion_through_entered_functions() {
    let src = "\
script
        CreateClosure depth
        StaGlobal depth
        LdaGlobal depth
        Star r1
        LdaSmi 200
        Star r2
        CallUndefined r1, r2, 1
        Return
end
function depth(n)               ; 3 loop iterations, then 1 + depth(n - 1)
        LdaSmi 0
        Star r2
loop:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 3, loop
        Ldar r1
        JumpIfLessSmi 1, base
        LdaGlobal depth
        Star r3
        Ldar r1
        AddSmi -1
        Star r4
        CallUndefined r3, r4, 1
        AddSmi 1
        Return
base:   LdaSmi 0
        Return
end
";
    assert_eq!(ok(src), "200");
}

#[test]
fn errors_in_entered_functions() {
    // `get(o)` has a loop; at the 30th call `o` is undefined.
    let src = "\
script
        CreateClosure get
        StaGlobal get
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function get(o)
        LdaSmi 0
        Star r2
loop:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 2, loop
        GetNamed r1, x
        Return
end
; r1 i, r2 get, r3 arg, r4 temp
function main()
        LdaGlobal get
        Star r2
        LdaSmi 0
        Star r1
top:    CreateArray
        Star r3
        Ldar r1
        TestEqualSmi 30
        JumpIfFalse call
        LdaUndefined
        Star r3
call:   CallUndefined r2, r3, 1
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 50, top
        Ldar r1
        Return
end
";
    let e = run(src).unwrap_err();
    assert_eq!(
        e.0,
        "TypeError: Cannot read properties of undefined (reading 'x')"
    );
}

// ---- Direct calls between compiled functions ----
//
// Callees with a loop, padded past the inlining limit (64 instructions),
// called from compiled loops: the call goes straight into the callee's
// compiled code, a version for the arguments' types.

/// `function name(k) { calls++; s = k; for (i = 0; i < 10; i++) s += i;
/// return s + 64 × step }`, padded with 64 `AddSmi step`.
fn padded(name: &str, step: i32) -> String {
    let pad: String = (0..64)
        .map(|_| format!("        AddSmi {step}\n"))
        .collect();
    format!(
        "function {name}(k)\n        LdaGlobal calls\n        Inc\n        StaGlobal calls\n\
         \x20       Ldar r1\n        Star r3\n        LdaSmi 0\n        Star r2\n\
         loop:   Ldar r2\n        Add r3\n        Star r3\n        Ldar r2\n        Inc\n\
         \x20       Star r2\n        JumpIfLessSmi 10, loop\n        Ldar r3\n{pad}        Return\nend\n"
    )
}

/// A script calling `main`, whose loop runs `n` times: `total += f(arg)`
/// with `f` from r3 and `arg = i & 7`, after `hook` (code run each
/// iteration, with `i` in r1); returns `total + calls`. Before the loop,
/// r5 holds 2147483600 and r6 the function `f2` (or `f1`, without one), so
/// that hooks change no inline cache the compiled loop depends on.
fn direct_program(n: i32, hook: &str, functions: &str) -> String {
    let second = if functions.contains("function f2(") {
        "f2"
    } else {
        "f1"
    };
    format!(
        "script\n        LdaSmi 0\n        StaGlobal calls\n\
         \x20       CreateClosure f1\n        StaGlobal f\n        CreateClosure {second}\n        StaGlobal g\n\
         \x20       CreateClosure main\n        StaGlobal main\n\
         \x20       LdaGlobal main\n        Star r1\n        CallUndefined r1, r2, 0\n        Return\nend\n\
         {functions}\
         function main()                 ; r1 i, r2 total, r3 f, r4 arg, r5 big, r6 g\n\
         \x20       LdaSmi 2147483600\n        Star r5\n        LdaGlobal g\n        Star r6\n\
         \x20       LdaGlobal f\n        Star r3\n\
         \x20       LdaSmi 0\n        Star r1\n        Star r2\n\
         top:    Ldar r1\n        BitAndSmi 7\n        Star r4\n\
         {hook}\
         call:   CallUndefined r3, r4, 1\n        Add r2\n        Star r2\n\
         \x20       Ldar r1\n        Inc\n        Star r1\n        JumpIfLessSmi {n}, top\n\
         \x20       LdaGlobal calls\n        Add r2\n        Return\nend\n"
    )
}

#[test]
fn direct_calls_into_compiled_callees() {
    let src = direct_program(500, "", &padded("f1", 1));
    // Σ (i & 7) + 45 + 64 over 500 iterations, plus 500 calls.
    let expected: u64 = (0..500u64).map(|i| (i & 7) + 109).sum::<u64>() + 500;
    let (v, s) = jit(&src);
    assert_eq!(v, expected.to_string());
    // The callee version returns int32s: the result is checked to be one,
    // and `total += f(…)` adds int32s.
    assert!(s.iter().any(|n| n == "call_direct+check_acc_int"), "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("add_ii_")), "{s:?}");
}

/// The argument is a double, so the callee version returns doubles, and
/// so does the call (checked to be a number, unboxed).
#[test]
fn direct_calls_returning_doubles() {
    let hook = "        Ldar r5\n        Add r5\n        Star r4\n";
    let src = direct_program(500, hook, &padded("f1", 1));
    let expected: u64 = 500 * (2 * 2147483600 + 109) + 500;
    let (v, s) = jit(&src);
    assert_eq!(v, expected.to_string());
    assert!(
        s.iter()
            .any(|n| n == "call_direct+check_acc_num+unbox_num_acc"),
        "{s:?}"
    );
}

/// A script whose `main` loops 500 times over `total += m.call(o, i).x`
/// with `o = [] + {x: 5}`, returning `total + calls`. `m` returns `this`
/// after `special` (code run first, with `k` in r1). The global `other`
/// is an object with `a = 1000` and then `x = 7`. The padding keeps `m`
/// from being inlined.
fn object_program(special: &str) -> String {
    let pad: String = (0..64).map(|_| "        AddSmi 1\n").collect();
    format!(
        "\
script
        LdaSmi 0
        StaGlobal calls
        CreateArray
        Star r3
        LdaSmi 1000
        SetNamed r3, a
        LdaSmi 7
        SetNamed r3, x
        Ldar r3
        StaGlobal other
        CreateClosure m
        StaGlobal m
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function m(k)
        LdaGlobal calls
        Inc
        StaGlobal calls
{special}        LdaSmi 0
        Star r2
loop:   Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
{pad}        Ldar r0
        Return
end
function main()                 ; r1 i, r2 total, r3 m, r7 o, r8 arg, r9 o again
        CreateArray
        Star r7
        LdaSmi 5
        SetNamed r7, x
        LdaGlobal m
        Star r3
        LdaSmi 0
        Star r1
        Star r2
top:    Ldar r1
        Star r8
        Call r3, r7, 1
        Star r9
        GetNamed r9, x
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 500, top
        LdaGlobal calls
        Add r2
        Return
end
"
    )
}

/// `m` returns `this`, an object of a known shape: the call's result is
/// checked to have it, and loading `x` from it needs no guard.
#[test]
fn direct_calls_returning_objects() {
    let (v, s) = jit(&object_program(""));
    assert_eq!(v, (500 * 5 + 500).to_string());
    assert!(
        s.iter().any(|n| n == "call_direct+check_acc_shape"),
        "{s:?}"
    );
    // `x` is out of line: arrays have no in-object slots.
    assert!(s.iter().any(|n| n.starts_with("get_oop_nc_")), "{s:?}");
}

/// At k = 300, `m` returns `other` instead, whose shape has `x` in another
/// slot: the check of the result's shape fails, and the interpreter loads
/// `x` (7, not `a`'s 1000).
#[test]
fn direct_call_results_are_checked() {
    let special = "        Ldar r1\n        TestEqualSmi 300\n        JumpIfFalse go\n\
                   \x20       LdaGlobal other\n        Return\ngo:\n";
    let (v, s) = jit(&object_program(special));
    assert_eq!(v, (499 * 5 + 7 + 500).to_string());
    assert!(s.iter().any(|n| n == "call_direct"), "{s:?}");
}

/// `f1` reads its local r2 before writing it, so a direct call must set it
/// to `undefined`: the previous call left `k` there. The caller writes
/// only what the callee may read first (here r1 and r2) into its frame.
#[test]
fn direct_calls_set_locals_read_first() {
    let pad: String = (0..64).map(|_| "        AddSmi 0\n").collect();
    let f1 = format!(
        "function f1(k)                  ; 1 if r2 is undefined on entry, else 1000\n\
         \x20       LdaUndefined\n        TestEqual r2\n        JumpIfFalse stale\n\
         \x20       LdaSmi 1\n        Jump done\n\
         stale:  LdaSmi 1000\n\
         done:   Star r3\n        Ldar r1\n        Star r2\n        LdaSmi 0\n        Star r4\n\
         loop:   Ldar r4\n        Inc\n        Star r4\n        JumpIfLessSmi 10, loop\n\
         \x20       Ldar r3\n{pad}        Return\nend\n"
    );
    let (v, s) = jit(&direct_program(500, "", &f1));
    assert_eq!(v, "500");
    assert!(s.iter().any(|n| n.starts_with("call_direct")), "{s:?}");
}

/// A script calling the recursive `name` (defined in `function`) with `n`.
fn recursive_program(name: &str, n: i32, function: &str) -> String {
    format!(
        "script\n        CreateClosure {name}\n        StaGlobal {name}\n        LdaGlobal {name}\n\
         \x20       Star r1\n        LdaSmi {n}\n        Star r2\n        CallUndefined r1, r2, 1\n\
         \x20       Return\nend\n{function}"
    )
}

/// Runs `src` with the JIT, checking the result against the interpreter's;
/// returns it and the stencils of `function`'s last compiled code.
fn jit_function(src: &str, function: &str) -> (String, Vec<String>) {
    let expected = ok(src);
    let m = toi::asm::parse(src).unwrap();
    let mut vm = Vm::new();
    let v = display(vm.run(&m).unwrap());
    assert_eq!(v, expected);
    (v, vm.jit_stencils(function).unwrap_or_default())
}

const FIB: &str = "\
function fib(n)
        Ldar r1
        JumpIfLessSmi 2, base
        LdaGlobal fib
        Star r2
        Ldar r1
        AddSmi -1
        Star r3
        CallUndefined r2, r3, 1
        Star r4
        Ldar r1
        AddSmi -2
        Star r3
        CallUndefined r2, r3, 1
        Add r4
        Return
base:   Ldar r1
        Return
end
";

/// `fib` names itself, so calls to it enter compiled code, where it calls
/// itself directly, with its results predicted to be int32s, as its other
/// `Return` gives (and then, checked, its own add does). The second call
/// hadn't run when `fib` first compiled: it bailed, and once it ran the
/// function compiled again. Both calls inline `fib` once, behind a callee
/// guard; the four calls in those copies are direct.
#[test]
fn direct_self_recursion() {
    let (v, s) = jit_function(&recursive_program("fib", 20, FIB), "fib");
    assert_eq!(v, "6765");
    let count = |stencil: &str| s.iter().filter(|n| n.starts_with(stencil)).count();
    assert_eq!(count("check_callee_"), 2, "{s:?}");
    assert_eq!(count("call_direct+check_acc_int"), 4, "{s:?}");
    assert!(s.iter().any(|n| n.starts_with("add_ii_")), "{s:?}");
}

/// `sum(n) = n + sum(n - 1)` from 2147483000: the add overflows int32 in
/// a callee called directly by the same code (its exit finishes in the
/// interpreter, from the code's own record), and the results are doubles
/// from then on. 15000 deep: past the native stack limit, the interpreter
/// makes the calls (without the limit, a 2 MiB test thread overflows).
#[test]
fn exits_in_direct_self_calls() {
    let sum = "\
function sum(n)
        Ldar r1
        JumpIfLessSmi 1, base
        LdaGlobal sum
        Star r2
        Ldar r1
        AddSmi -1
        Star r3
        CallUndefined r2, r3, 1
        Add r1
        Return
base:   LdaSmi 2147483000
        Return
end
";
    let (v, _) = jit_function(&recursive_program("sum", 15000, sum), "sum");
    assert_eq!(v, (2147483000u64 + 15000 * 15001 / 2).to_string());
}

/// A loop over `total += new Box(i).v + a.push(i)` (`box_body` builds the
/// object, with `k` in r1), returning `total`.
fn runtime_calls_program(box_body: &str) -> String {
    format!(
        "\
script
        CreateClosure Box
        StaGlobal Box
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function Box(k)
{box_body}        LdaUndefined
        Return
end
function main()                 ; r1 i, r2 total, r3 a, r4-r8 temps
        CreateArray
        Star r3
        LdaSmi 0
        Star r1
        Star r2
loop:   LdaGlobal Box
        Star r4
        Ldar r1
        Star r5
        Construct r4, r5, 1
        Star r8
        GetNamed r8, v
        Add r2
        Star r2
        GetNamed r3, push
        Star r6
        Mov r3, r7
        Ldar r1
        Star r8
        Call r6, r7, 1
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 300, loop
        Ldar r2
        Return
end
"
    )
}

/// `new Box(i)` (a direct call) is predicted to give an object of the shape
/// `Box`'s objects have had, so loading `v` needs no guard, and `push`
/// (through the runtime) an int32, as natives declare, so the add is on
/// int32s. `Box` itself is compiled knowing `this`'s shape: storing `v`
/// is a transition without a guard.
#[test]
fn runtime_call_results_are_predicted() {
    let src = runtime_calls_program("        Ldar r1\n        SetNamed r0, v\n");
    // Σ i + Σ (i + 1), the array's length after each push.
    let (v, s) = jit(&src);
    assert_eq!(v, (300 * 300).to_string());
    assert!(
        s.iter().any(|n| n == "construct_direct+check_acc_shape"),
        "{s:?}"
    );
    assert!(s.iter().any(|n| n.starts_with("get_own_nc_")), "{s:?}");
    assert!(s.iter().any(|n| n == "call+check_acc_int"), "{s:?}");
    assert!(
        s.iter().filter(|n| n.starts_with("add_ii_")).count() >= 2,
        "{s:?}"
    );
    let m = toi::asm::parse(&src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(2);
    vm.run(&m).unwrap();
    let s = vm
        .jit_stencils("Box")
        .expect("Box compiled for the direct call");
    assert!(s.iter().any(|n| n.starts_with("set_tr_nc_")), "{s:?}");
    assert_eq!(vm.jit_bound_direct_calls("main"), 1);
}

/// From k = 148, `Box`'s `k + 2147483500` overflows int32 and its code
/// (entered directly) exits: the interpreter finishes the `new` frame
/// (the exit writes its header with the `new` flag), so the result is
/// still `this`, not `Box`'s `undefined`.
#[test]
fn exits_in_direct_constructors() {
    let body = "        Ldar r1\n        AddSmi 2147483500\n        SetNamed r0, v\n";
    let (v, _) = jit(&runtime_calls_program(body));
    let expected: u64 = 300 * 2147483500 + (0..300).sum::<u64>() + (1..=300).sum::<u64>();
    assert_eq!(v, expected.to_string());
}

/// From k = 200, `Box` gives its objects 5 more properties, past the
/// in-object capacity: `new` then allocates `this` from another root, not
/// the shape `Box`'s version was bound for, and goes through the runtime.
#[test]
fn direct_constructors_whose_root_changes() {
    let mut body = String::from(
        "        Ldar r1\n        SetNamed r0, v\n        Ldar r1\n        JumpIfLessSmi 200, done\n",
    );
    for p in ["a", "b", "c", "d", "e"] {
        body += &format!("        LdaSmi 0\n        SetNamed r0, {p}\n");
    }
    body += "done:\n";
    let (v, _) = jit(&runtime_calls_program(&body));
    assert_eq!(v, (300 * 300).to_string());
}

/// The loop sets `Box.prototype` to `other` (whose `w` is 10; the first
/// prototype's is 1) at i = 100, to `last` (1000) at 200, and to `other`
/// again at 250. The first two exit the loop's code (their stores hadn't
/// run), which compiles again, binding `new Box` for the new root. The
/// third runs compiled, and the field's type is general by then: nothing
/// is compiled again. `new Box` then allocates `this` from another root,
/// with the other prototype, not the shape `Box`'s version was bound for,
/// and goes through the runtime. Entering the version would give the
/// object the old prototype back.
#[test]
fn direct_constructors_see_prototype_changes() {
    let src = "\
script
        CreateClosure Box
        StaGlobal Box
        LdaGlobal Box
        Star r1
        GetNamed r1, prototype
        Star r2
        LdaSmi 1
        SetNamed r2, w
        CreateArray
        Star r1
        LdaSmi 10
        SetNamed r1, w
        Ldar r1
        StaGlobal other
        CreateArray
        Star r1
        LdaSmi 1000
        SetNamed r1, w
        Ldar r1
        StaGlobal last
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function Box(k)
        Ldar r1
        SetNamed r0, v
        LdaUndefined
        Return
end
function main()                 ; r1 i, r2 total
        LdaSmi 0
        Star r1
        Star r2
loop:   Ldar r1
        TestEqualSmi 100
        JumpIfTrue first
        Ldar r1
        TestEqualSmi 250
        JumpIfFalse second
first:  LdaGlobal Box
        Star r3
        LdaGlobal other
        SetNamed r3, prototype
second: Ldar r1
        TestEqualSmi 200
        JumpIfFalse new
        LdaGlobal Box
        Star r3
        LdaGlobal last
        SetNamed r3, prototype
new:    LdaGlobal Box
        Star r4
        Ldar r1
        Star r5
        Construct r4, r5, 1
        Star r6
        GetNamed r6, w
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 300, loop
        Ldar r2
        Return
end
";
    let (v, _) = jit(src);
    assert_eq!(v, (100 + 100 * 10 + 50 * 1000 + 50 * 10).to_string());
}

/// A compiled loop calls `new F(3)` directly, `F` tail-calling itself and
/// returning `this` (see `self_tail_calls_keep_new_frames`). The tail call
/// may reuse the frame, overwriting `this` there: `toi_rt_construct` keeps
/// its own, so the result is never `undefined`.
#[test]
fn direct_constructors_keep_their_frames() {
    let src = "\
script
        CreateClosure F
        StaGlobal F
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function F(n)
        Ldar r1
        JumpIfLessSmi 1, done
        LdaGlobal F
        Star r2
        Ldar r1
        AddSmi -1
        Star r3
        CallUndefined r2, r3, 1
        Return
done:   Ldar r0
        Return
end
function main()                 ; r1 i, r2 how many results were undefined
        LdaSmi 0
        Star r1
        Star r2
loop:   LdaGlobal F
        Star r4
        LdaSmi 3
        Star r5
        CallUndefined r4, r5, 1
        LdaGlobal F
        Star r4
        LdaSmi 3
        Star r5
        Construct r4, r5, 1
        Star r6
        LdaUndefined
        TestEqual r6
        JumpIfFalse next
        Ldar r2
        Inc
        Star r2
next:   Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 100, loop
        Ldar r2
        Return
end
";
    let (v, s) = jit(src);
    assert_eq!(v, "0");
    assert!(s.iter().any(|n| n.starts_with("construct_direct")), "{s:?}");
}

/// For odd `k`, `R` returns another object (`other`, whose `v` is 1000)
/// instead of `this`: that is the result of `new`, called directly. Its
/// shape isn't `this`'s, so the result's check fails, and the call is
/// untyped from then on.
#[test]
fn direct_constructors_returning_objects() {
    let src = "\
script
        CreateArray
        Star r1
        LdaSmi 1000
        SetNamed r1, v
        Ldar r1
        StaGlobal other
        CreateClosure R
        StaGlobal R
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function R(k)
        Ldar r1
        BitAndSmi 1
        TestEqualSmi 1
        JumpIfFalse even
        LdaGlobal other
        Return
even:   Ldar r1
        SetNamed r0, v
        LdaUndefined
        Return
end
function main()                 ; r1 i, r2 total
        LdaSmi 0
        Star r1
        Star r2
loop:   LdaGlobal R
        Star r4
        Ldar r1
        Star r5
        Construct r4, r5, 1
        Star r6
        GetNamed r6, v
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 300, loop
        Ldar r2
        Return
end
";
    let (v, s) = jit(src);
    let even: i32 = (0..300).step_by(2).sum();
    assert_eq!(v, (even + 150 * 1000).to_string());
    assert!(s.iter().any(|n| n == "construct_direct"), "{s:?}");
}

/// From k = 200, `Box` adds `w` first: its objects get another shape, the
/// check of `new`'s result fails, and the call is compiled again without a
/// prediction (the constructor's shapes are many now anyway).
#[test]
fn runtime_call_results_are_checked() {
    let body = "        Ldar r1\n        JumpIfLessSmi 200, v\n        LdaSmi 0\n        SetNamed r0, w\n\
                v:      Ldar r1\n        SetNamed r0, v\n";
    let (v, s) = jit(&runtime_calls_program(body));
    assert_eq!(v, (300 * 300).to_string());
    assert!(!s.iter().any(|n| n == "call+check_acc_shape"), "{s:?}");
}

/// `a` calls `b`, which calls `c`: in `main`'s loop, `a` and `b` are
/// inlined, but not `c`, nested too deep. Small as it is, `c` is called
/// directly (calls from the interpreter wouldn't enter its compiled code).
#[test]
fn small_callees_past_the_inlining_depth_are_called_directly() {
    let f = |name: &str, callee: &str| {
        format!(
            "function {name}(k)\n        LdaGlobal {callee}\n        Star r2\n        Ldar r1\n\
             \x20       Star r3\n        CallUndefined r2, r3, 1\n        AddSmi 1\n        Return\nend\n"
        )
    };
    let src = format!(
        "script\n        CreateClosure a\n        StaGlobal a\n        CreateClosure b\n\
         \x20       StaGlobal b\n        CreateClosure c\n        StaGlobal c\n\
         \x20       CreateClosure main\n        StaGlobal main\n        LdaGlobal main\n\
         \x20       Star r1\n        CallUndefined r1, r2, 0\n        Return\nend\n\
         {}{}function c(k)\n        Ldar r1\n        AddSmi 1\n        Return\nend\n\
         function main()                 ; r1 i, r2 total, r3 a, r4 arg\n\
         \x20       LdaSmi 0\n        Star r1\n        Star r2\n\
         top:    LdaGlobal a\n        Star r3\n        Ldar r1\n        BitAndSmi 7\n\
         \x20       Star r4\n        CallUndefined r3, r4, 1\n        Add r2\n        Star r2\n\
         \x20       Ldar r1\n        Inc\n        Star r1\n        JumpIfLessSmi 500, top\n\
         \x20       Ldar r2\n        Return\nend\n",
        f("a", "b"),
        f("b", "c")
    );
    let expected: u64 = (0..500u64).map(|i| (i & 7) + 3).sum();
    let (v, s) = jit(&src);
    assert_eq!(v, expected.to_string());
    assert!(s.iter().any(|n| n == "call_direct+check_acc_int"), "{s:?}");
}

/// When `k` is 7 (every 8th call), `f1` calls `h`, which it hadn't when
/// it was first compiled: that version leaves there. The exit makes `f1`
/// compile again, and the direct call site binds the new version at the
/// next call, instead of calling the old one until it gives up.
#[test]
fn direct_calls_rebind_after_callee_exits() {
    let pad: String = (0..64).map(|_| "        AddSmi 1\n").collect();
    let src = format!(
        "\
script
        CreateClosure f1
        StaGlobal f
        CreateClosure h
        StaGlobal h
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end
function h()
        LdaSmi 0
        Return
end
function f1(k)                  ; k + (0 + … + 9) + 64, calling h() if k is 7
        Ldar r1
        TestEqualSmi 7
        JumpIfFalse go
        LdaGlobal h
        Star r5
        CallUndefined r5, r6, 0
go:     Ldar r1
        Star r3
        LdaSmi 0
        Star r2
loop:   Ldar r2
        Add r3
        Star r3
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 10, loop
        Ldar r3
{pad}        Return
end
function main()                 ; r1 i, r2 total, r3 f, r4 arg
        LdaGlobal f
        Star r3
        LdaSmi 0
        Star r1
        Star r2
top:    Ldar r1
        BitAndSmi 7
        Star r4
        CallUndefined r3, r4, 1
        Add r2
        Star r2
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 500, top
        Ldar r2
        Return
end
"
    );
    let expected: u64 = (0..500u64).map(|i| (i & 7) + 109).sum();
    let m = toi::asm::parse(&src).unwrap();
    let mut vm = Vm::new();
    vm.set_tier_back_edges(2);
    assert_eq!(display(vm.run(&m).unwrap()), expected.to_string());
    assert_eq!(ok(&src), expected.to_string());
    assert_eq!(vm.jit_bound_direct_calls("main"), 1);
}

const TAIL_SUM: &str = "\
function sum(n, acc)
        Ldar r1
        JumpIfLessSmi 1, done
        LdaGlobal sum
        Star r3
        Ldar r1
        AddSmi -1
        Star r4
        Ldar r2
        Add r1
        Star r5
        CallUndefined r3, r4, 2
        Return
done:   Ldar r2
        Return
end
";

/// A script calling `sum(n, 0)`.
fn tail_sum_program(n: i32) -> String {
    format!(
        "script\n        CreateClosure sum\n        StaGlobal sum\n        LdaGlobal sum\n\
         \x20       Star r1\n        LdaSmi {n}\n        Star r2\n        LdaSmi 0\n        Star r3\n\
         \x20       CallUndefined r1, r2, 2\n        Return\nend\n{TAIL_SUM}"
    )
}

/// `return sum(n - 1, acc + n)` calls the function itself and returns the
/// result: compiled code moves the arguments into the parameters and jumps
/// back to the start, behind a guard that `sum` is still the callee.
#[test]
fn self_tail_calls_are_loops() {
    let (v, s) = jit_function(&tail_sum_program(10000), "sum");
    assert_eq!(v, (10000 * 10001 / 2).to_string());
    assert!(
        s.iter().any(|n| n.starts_with("check_tail_callee_")),
        "{s:?}"
    );
    assert!(!s.iter().any(|n| n.starts_with("call")), "{s:?}");
}

/// A million deep: the interpreter runs out of VM stack, but the compiled
/// loop runs in constant space (and `acc` goes past int32, so it widens
/// to a double).
#[test]
fn deep_self_tail_recursion() {
    let m = toi::asm::parse(&tail_sum_program(1_000_000)).unwrap();
    let mut vm = Vm::new();
    assert_eq!(display(vm.run(&m).unwrap()), "500000500000");
    let mut vm = Vm::new();
    vm.set_tier_back_edges(i64::MAX);
    let e = vm.run(&m).unwrap_err();
    assert_eq!(e.0, "RangeError: Maximum call stack size exceeded");
}

/// The arguments' registers overlap the parameters (`f(n - 1, k + 1)` with
/// the arguments in r2 and r3): moved first to last, each is read before
/// it's overwritten.
#[test]
fn self_tail_calls_move_overlapping_arguments() {
    let src = "\
script
        CreateClosure f
        StaGlobal f
        LdaGlobal f
        Star r1
        LdaSmi 300
        Star r2
        LdaSmi 0
        Star r3
        CallUndefined r1, r2, 2
        Return
end
function f(n, k)                ; k + n
        Ldar r1
        JumpIfLessSmi 1, done
        LdaGlobal f
        Star r4
        Ldar r2
        AddSmi 1
        Star r3
        Ldar r1
        AddSmi -1
        Star r2
        CallUndefined r4, r2, 2
        Return
done:   Ldar r2
        Return
end
";
    let (v, s) = jit_function(src, "f");
    assert_eq!(v, "300");
    assert!(
        s.iter().any(|n| n.starts_with("check_tail_callee_")),
        "{s:?}"
    );
}

/// `F` tail-calls itself, and is compiled by plain calls; at the end it
/// returns `this` (`undefined` in plain calls). `new F(3)` must not reuse
/// its frame: the result of a `new` is its `this` when the constructor
/// returns a non-object, and the tail call (whose `this` is `undefined`)
/// would overwrite it. The guard sees the frame's `new` flag and leaves,
/// and the interpreter makes the call.
#[test]
fn self_tail_calls_keep_new_frames() {
    let src = "\
script                          ; 20 × F(5), then new F(3) == undefined
        CreateClosure F
        StaGlobal F
        LdaSmi 0
        Star r4
warm:   LdaGlobal F
        Star r1
        LdaSmi 5
        Star r2
        CallUndefined r1, r2, 1
        Ldar r4
        Inc
        Star r4
        JumpIfLessSmi 20, warm
        LdaGlobal F
        Star r1
        LdaSmi 3
        Star r2
        Construct r1, r2, 1
        Star r3
        LdaUndefined
        TestEqual r3
        Return
end
function F(n)
        Ldar r1
        JumpIfLessSmi 1, done
        LdaGlobal F
        Star r2
        Ldar r1
        AddSmi -1
        Star r3
        CallUndefined r2, r3, 1
        Return
done:   Ldar r0
        Return
end
";
    let (v, s) = jit_function(src, "F");
    assert_eq!(v, "false");
    assert!(
        s.iter().any(|n| n.starts_with("check_tail_callee_")),
        "{s:?}"
    );
}

/// `fib` split over `f` and `g`, which call each other (`pad` after their
/// base cases), and a script calling `f(n)`.
fn mutual_program(n: i32, pad: usize) -> String {
    let pad: String = (0..pad).map(|_| "        AddSmi 0\n").collect();
    let function = |name: &str, other: &str| {
        format!(
            "function {name}(n)\n        Ldar r1\n        JumpIfLessSmi 2, base\n\
             \x20       LdaGlobal {other}\n        Star r2\n        Ldar r1\n        AddSmi -1\n\
             \x20       Star r3\n        CallUndefined r2, r3, 1\n        Star r4\n        Ldar r1\n\
             \x20       AddSmi -2\n        Star r3\n        CallUndefined r2, r3, 1\n        Add r4\n\
             \x20       Return\nbase:   Ldar r1\n{pad}        Return\nend\n"
        )
    };
    format!(
        "script\n        CreateClosure f\n        StaGlobal f\n        CreateClosure g\n\
         \x20       StaGlobal g\n        LdaGlobal f\n        Star r1\n        LdaSmi {n}\n\
         \x20       Star r2\n        CallUndefined r1, r2, 1\n        Return\nend\n{}{}",
        function("f", "g"),
        function("g", "f")
    )
}

/// `f` and `g` call each other, so both are compiled at entry. `g` is
/// small: inlined into `f`, whose calls (from the inlined `g`) are to
/// itself.
#[test]
fn mutual_recursion_inlined() {
    let (v, s) = jit_function(&mutual_program(20, 0), "f");
    assert_eq!(v, "6765");
    let typed = s
        .iter()
        .filter(|n| *n == "call_direct+check_acc_int")
        .count();
    assert!(typed >= 2, "{s:?}");
}

/// Too big to inline, `f` and `g` call each other directly. When `f`
/// compiles, binding its calls compiles `g`, whose calls to `f` can't bind
/// yet (`f` is being compiled): they bind at their first call. Their
/// results are predicted from `f`'s prediction for itself (`ret_hint`),
/// so all four calls are typed.
#[test]
fn mutual_recursion_direct() {
    let src = mutual_program(20, 64);
    let expected = ok(&src);
    let m = toi::asm::parse(&src).unwrap();
    let mut vm = Vm::new();
    assert_eq!(display(vm.run(&m).unwrap()), expected);
    assert_eq!(expected, "6765");
    for name in ["f", "g"] {
        let s = vm.jit_stencils(name).unwrap_or_default();
        let typed = s
            .iter()
            .filter(|n| *n == "call_direct+check_acc_int")
            .count();
        assert_eq!(typed, 2, "{name}: {s:?}");
        assert_eq!(vm.jit_bound_direct_calls(name), 2, "{name}");
    }
}

/// At i = 300 the argument is near the int32 limit: the direct callee's
/// add overflows and exits, inside the callee; the runtime finishes that
/// call (once: `calls` would show a second) and later calls work on.
#[test]
fn exits_in_direct_callees_finish_in_the_interpreter() {
    let hook = "        Ldar r1\n        TestEqualSmi 300\n        JumpIfFalse call\n\
                \x20       Ldar r5\n        Star r4\n";
    let src = direct_program(500, hook, &padded("f1", 1));
    let expected: u64 = (0..500u64)
        .map(|i| {
            if i == 300 {
                2147483600 + 109
            } else {
                (i & 7) + 109
            }
        })
        .sum::<u64>()
        + 500;
    let (v, s) = jit(&src);
    assert_eq!(v, expected.to_string());
    // The call's result was a double then, failing the check that it was
    // an int32: the call is compiled again, without a prediction.
    assert!(s.iter().any(|n| n == "call_direct"), "{s:?}");
}

/// From i = 300 the callee is another function: the direct call's check of
/// the callee fails, and the call goes through the runtime.
#[test]
fn direct_calls_check_the_callee() {
    let hook = "        Ldar r1\n        TestEqualSmi 300\n        JumpIfFalse call\n\
                \x20       Ldar r6\n        Star r3\n";
    let functions = padded("f1", 1) + &padded("f2", 2);
    let src = direct_program(500, hook, &functions);
    let expected: u64 = (0..500u64)
        .map(|i| (i & 7) + 45 + if i < 300 { 64 } else { 128 })
        .sum::<u64>()
        + 500;
    assert_eq!(jit(&src).0, expected.to_string());
}

#[test]
fn errors_in_direct_callees() {
    // At i = 300, f is called with undefined, and reads a property of it.
    let pad: String = (0..64).map(|_| "        AddSmi 1\n").collect();
    let f1 = format!(
        "function f1(k)\n        LdaSmi 0\n        Star r2\n\
         loop:   Ldar r2\n        Inc\n        Star r2\n        JumpIfLessSmi 3, loop\n\
         \x20       Ldar r1\n        JumpIfToBooleanTrue num\n        GetNamed r1, x\n\
         num:    Ldar r2\n{pad}        Return\nend\n"
    );
    let hook = "        Ldar r1\n        TestEqualSmi 300\n        JumpIfFalse call\n\
                \x20       LdaUndefined\n        Star r4\n";
    let src = direct_program(500, hook, &f1);
    let e = run(&src).unwrap_err();
    assert_eq!(
        e.0,
        "TypeError: Cannot read properties of undefined (reading 'x')"
    );
}
