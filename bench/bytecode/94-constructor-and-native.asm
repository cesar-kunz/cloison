; Expected bytecode for bench/micro/94-constructor-and-native.js.
;
;   function Box(k) { this.v = k; }
;   function main() {
;     var a = [], s = 0;
;     for (var i = 0; i < 200000; i++) { s = s + new Box(i).v; s = s + a.push(i); }
;     return s;
;   }
;   main();

script                          ; r1 callee
        CreateClosure Box
        StaGlobal Box
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function Box(k)                 ; r0 this, r1 k
        Ldar r1
        SetNamed r0, v
        LdaUndefined
        Return
end

function main()                 ; r1 a, r2 s, r3 i, r4 callee then object, r5-r6 window
        CreateArray 0
        Star r1                 ; a = []
        LdaSmi 0
        Star r2                 ; s = 0
        Star r3                 ; i = 0
loop:   LdaGlobal Box
        Star r4
        Construct r4, r3, 1     ; the argument in place
        Star r4
        GetNamed r4, v
        Add r2
        Star r2                 ; s = s + new Box(i).v
        GetNamed r1, push
        Star r4
        Mov r1, r5              ; this = a
        Mov r3, r6
        Call r4, r5, 1
        Add r2
        Star r2                 ; s = s + a.push(i)
        Ldar r3
        Inc
        Star r3
        JumpIfLessSmi 200000, loop
        Ldar r2
        Return
end
