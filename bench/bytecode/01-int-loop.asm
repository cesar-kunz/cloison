; Expected bytecode for bench/micro/01-int-loop.js.
;
;   function main() {
;     var sum = 0;
;     for (var i = 0; i < 1000000; i++) sum = (sum + i) | 0;
;     return sum;
;   }
;   main();

script                          ; r1 callee
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function main()                 ; r1 sum, r2 i
        LdaSmi 0
        Star r1                 ; sum = 0
        Star r2                 ; i = 0
        Jump test
body:   Ldar r2
        Add r1                  ; sum + i
        BitOrSmi 0
        Star r1
        Ldar r2
        Inc
        Star r2                 ; i++
test:   Ldar r2
        JumpIfLessSmi 1000000, body
        Ldar r1
        Return
end
