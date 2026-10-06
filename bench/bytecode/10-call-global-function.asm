; Expected bytecode for bench/micro/10-call-global-function.js.
;
;   function less(a, b) { return a < b; }
;   function main() {
;     var n = 0;
;     for (var i = 0; i < 1000000; i++) if (less(i % 13, i % 7)) n++;
;     return n;
;   }
;   main();

script                          ; r1 callee
        CreateClosure less
        StaGlobal less
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function less(a, b)             ; r1 a, r2 b
        Ldar r2
        TestLessThan r1         ; a < b
        Return
end

function main()                 ; r1 n, r2 i, r3 callee, r4-r5 arguments
        LdaSmi 0
        Star r1                 ; n = 0
        Star r2                 ; i = 0
        Jump test
body:   LdaGlobal less
        Star r3
        Ldar r2
        ModSmi 13
        Star r4
        Ldar r2
        ModSmi 7
        Star r5
        CallUndefined r3, r4, 2 ; less(i % 13, i % 7)
        JumpIfToBooleanFalse next
        Ldar r1
        Inc
        Star r1                 ; n++
next:   Ldar r2
        Inc
        Star r2                 ; i++
test:   Ldar r2
        JumpIfLessSmi 1000000, body
        Ldar r1
        Return
end
