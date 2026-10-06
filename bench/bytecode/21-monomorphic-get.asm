; Expected bytecode for bench/micro/21-monomorphic-get.js.
;
;   function V(i) { this.value = i; this.mark = i % 5; this.fixed = (i & 1) == 0; }
;   function main() {
;     var vs = [];
;     for (var i = 0; i < 100; i++) vs.push(new V(i));
;     var n = 0;
;     for (var r = 0; r < 10000; r++) {
;       for (var i = 0; i < 100; i++) {
;         var v = vs[i];
;         if (v.mark == (r % 5) || v.fixed) n += v.value;
;       }
;     }
;     return n;
;   }
;   main();

script                          ; r1 callee
        CreateClosure V
        StaGlobal V
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function V(i)                   ; r0 this, r1 i
        Ldar r1
        SetNamed r0, value      ; this.value = i
        Ldar r1
        ModSmi 5
        SetNamed r0, mark       ; this.mark = i % 5
        Ldar r1
        BitAndSmi 1
        TestEqualSmi 0
        SetNamed r0, fixed      ; this.fixed = (i & 1) == 0
        LdaUndefined
        Return
end

function main()                 ; r1 vs, r2 i, r3 n, r4 r, r5 v, r6-r9 temporaries
        CreateArray
        Star r1                 ; vs = []
        LdaSmi 0
        Star r2                 ; i = 0
        Jump test1
fill:   GetNamed r1, push
        Star r6
        Mov r1, r7              ; this = vs
        LdaGlobal V
        Star r9
        Construct r9, r2, 1     ; new V(i)
        Star r8
        Call r6, r7, 1          ; vs.push(…)
        Ldar r2
        Inc
        Star r2                 ; i++
test1:  Ldar r2
        JumpIfLessSmi 100, fill
        LdaSmi 0
        Star r3                 ; n = 0
        Star r4                 ; r = 0
        Jump test2
outer:  LdaSmi 0
        Star r2                 ; i = 0
        Jump test3
inner:  Ldar r2
        GetKeyed r1
        Star r5                 ; v = vs[i]
        GetNamed r5, mark
        Star r6
        Ldar r4
        ModSmi 5
        TestEqual r6            ; v.mark == (r % 5)
        JumpIfTrue add
        GetNamed r5, fixed
        JumpIfToBooleanFalse skip
add:    GetNamed r5, value
        Add r3
        Star r3                 ; n += v.value
skip:   Ldar r2
        Inc
        Star r2                 ; i++
test3:  Ldar r2
        JumpIfLessSmi 100, inner
        Ldar r4
        Inc
        Star r4                 ; r++
test2:  Ldar r4
        JumpIfLessSmi 10000, outer
        Ldar r3
        Return
end
