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

function main()                 ; r1 i, r2 s, r3 constructor, r4-r6 arguments, r7 object
        LdaSmi 0
        Star r1                 ; i = 0
        Star r2                 ; s = 0
loop:   LdaGlobal Point
        Star r3
        Ldar r1
        Star r4
        Star r5
        Star r6
        Construct r3, r4, 3
        Star r7
        GetNamed r7, z
        Add r2
        Star r2                 ; s = s + new Point(i, i, i).z
        Ldar r1
        Inc
        Star r1
        JumpIfLessSmi 200000, loop
        Ldar r2
        Return
end
