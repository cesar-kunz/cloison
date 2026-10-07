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

function fib(n)                 ; r1 n, r2 callee, r3 argument, r4 fib(n - 1)
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
        Add r4                  ; fib(n - 1) + fib(n - 2)
        Return
base:   Ldar r1
        Return
end
