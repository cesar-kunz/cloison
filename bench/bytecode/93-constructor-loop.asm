; Expected bytecode for bench/micro/93-constructor-loop.js.
;
;   function Point(x, y, z) { this.x = x; this.y = y; this.z = z; }
;   function main() {
;     var s = 0;
;     for (var i = 0; i < 200000; i++) s = s + new Point(i, i, i).z;
;     return s;
;   }
;   main();

script                          ; r1 callee
        CreateClosure Point
        StaGlobal Point
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function Point(x, y, z)         ; r0 this, r1 x, r2 y, r3 z
        Ldar r1
        SetNamed r0, x
        Ldar r2
        SetNamed r0, y
        Ldar r3
        SetNamed r0, z
        LdaUndefined
        Return
end

function main()                 ; r1 s, r2 i, r3 constructor then object, r4-r6 arguments
        LdaSmi 0
        Star r1                 ; s = 0
        Star r2                 ; i = 0
loop:   LdaGlobal Point
        Star r3
        Mov r2, r4
        Mov r2, r5
        Mov r2, r6
        Construct r3, r4, 3
        Star r3
        GetNamed r3, z
        Add r1
        Star r1                 ; s = s + new Point(i, i, i).z
        Ldar r2
        Inc
        Star r2
        JumpIfLessSmi 200000, loop
        Ldar r1
        Return
end
