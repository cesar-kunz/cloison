; Expected bytecode for bench/micro/91-mutual-recursion.js.
;
;   function f(n) { if (n < 2) return n; return g(n - 1) + g(n - 2); }
;   function g(n) { if (n < 2) return n; return f(n - 1) + f(n - 2); }
;   f(27);

script                          ; r1 callee, r2 argument
        CreateClosure f
        StaGlobal f
        CreateClosure g
        StaGlobal g
        LdaGlobal f
        Star r1
        LdaSmi 27
        Star r2
        CallUndefined r1, r2, 1
        Return
end

function f(n)                   ; r1 n, r2 callee, r3 argument, r4 g(n - 1)
        Ldar r1
        JumpIfLessSmi 2, base
        LdaGlobal g
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

function g(n)                   ; r1 n, r2 callee, r3 argument, r4 f(n - 1)
        Ldar r1
        JumpIfLessSmi 2, base
        LdaGlobal f
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
