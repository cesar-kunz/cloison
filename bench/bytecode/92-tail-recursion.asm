; Expected bytecode for bench/micro/92-tail-recursion.js.
;
;   function sum(n, acc) { if (n < 1) return acc; return sum(n - 1, acc + n); }
;   function main() {
;     var t = 0;
;     for (var i = 0; i < 100; i++) t = t + sum(5000, 0);
;     return t;
;   }
;   main();

script                          ; r1 callee
        CreateClosure sum
        StaGlobal sum
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function sum(n, acc)            ; r1 n, r2 acc, r3 callee, r4-r5 arguments
        Ldar r1
        JumpIfLessSmi 1, done
        LdaGlobal sum
        Star r3
        Ldar r1
        AddSmi -1
        Star r4                 ; n - 1
        Ldar r1
        Add r2
        Star r5                 ; acc + n
        CallUndefined r3, r4, 2
        Return
done:   Ldar r2
        Return
end

function main()                 ; r1 t, r2 i, r3 callee, r4-r5 arguments
        LdaSmi 0
        Star r1                 ; t = 0
        Star r2                 ; i = 0
loop:   LdaGlobal sum
        Star r3
        LdaSmi 5000
        Star r4
        LdaSmi 0
        Star r5
        CallUndefined r3, r4, 2
        Add r1
        Star r1                 ; t = t + sum(5000, 0)
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 100, loop
        Ldar r1
        Return
end
