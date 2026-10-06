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
    let src = with_main(
        "function main()\n LdaGlobal main\n Star r1\n CallUndefined r1, r2, 0\n Return\nend\n",
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
