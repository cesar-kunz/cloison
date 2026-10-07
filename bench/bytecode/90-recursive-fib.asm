; Expected bytecode for bench/micro/90-recursive-fib.js.
;
;   function fib(n) { if (n < 2) return n; return fib(n - 1) + fib(n - 2); }
;   fib(27);

script                          ; r1 callee, r2 argument
        CreateClosure fib
        StaGlobal fib
        LdaGlobal fib
        Star r1
        LdaSmi 27
        Star r2
        CallUndefined r1, r2, 1
        Return
end

function fib(n)                 ; r1 n, r2 callee, then fib(n - 1), r3 callee, r4 argument
        Ldar r1
        JumpIfLessSmi 2, base
        LdaGlobal fib
        Star r2
        Ldar r1
        SubSmi 1
        Star r3
        CallUndefined r2, r3, 1
        Star r2                 ; fib(n - 1)
        LdaGlobal fib           ; again: the call may have changed it
        Star r3
        Ldar r1
        SubSmi 2
        Star r4
        CallUndefined r3, r4, 1
        Add r2                  ; fib(n - 1) + fib(n - 2)
        Return
base:   Ldar r1
        Return
end
