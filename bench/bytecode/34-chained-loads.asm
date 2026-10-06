; Expected bytecode for bench/micro/34-chained-loads.js.
;
;   function S(v) { this.value = v; }
;   function V(s) { this.level = s; }
;   function C(s, v1, v2) { this.rank = s; this.v1 = v1; this.v2 = v2; }
;   function main() {
;     var s0 = new S(0), s3 = new S(3), s5 = new S(5);
;     var c = new C(s3, new V(s0), new V(s5)), n = 0;
;     for (var i = 0; i < 1000000; i++) {
;       if (c.rank.value < c.v2.level.value) n++;
;       if (c.v1.level.value < c.rank.value) n += 2;
;       if ((i & 1023) == 0) { var t = c.v1; c.v1 = c.v2; c.v2 = t; }
;     }
;     return n;
;   }
;   main();

script                          ; r1 callee
        CreateClosure S
        StaGlobal S
        CreateClosure V
        StaGlobal V
        CreateClosure C
        StaGlobal C
        CreateClosure main
        StaGlobal main
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function S(v)                   ; r0 this, r1 v
        Ldar r1
        SetNamed r0, value
        LdaUndefined
        Return
end

function V(s)                   ; r0 this, r1 s
        Ldar r1
        SetNamed r0, level
        LdaUndefined
        Return
end

function C(s, v1, v2)           ; r0 this, r1 s, r2 v1, r3 v2
        Ldar r1
        SetNamed r0, rank
        Ldar r2
        SetNamed r0, v1
        Ldar r3
        SetNamed r0, v2
        LdaUndefined
        Return
end

; r1 s0, r2 s3, r3 s5, r4 c, r5 n, r6 i, r7 t, r8-r12 temporaries
function main()
        LdaGlobal S
        Star r8
        LdaSmi 0
        Star r9
        Construct r8, r9, 1
        Star r1                 ; s0 = new S(0)
        LdaGlobal S
        Star r8
        LdaSmi 3
        Star r9
        Construct r8, r9, 1
        Star r2                 ; s3 = new S(3)
        LdaGlobal S
        Star r8
        LdaSmi 5
        Star r9
        Construct r8, r9, 1
        Star r3                 ; s5 = new S(5)
        LdaGlobal C
        Star r8
        Mov r2, r9
        LdaGlobal V
        Star r12
        Construct r12, r1, 1    ; new V(s0)
        Star r10
        LdaGlobal V
        Star r12
        Construct r12, r3, 1    ; new V(s5)
        Star r11
        Construct r8, r9, 3
        Star r4                 ; c = new C(s3, …, …)
        LdaSmi 0
        Star r5                 ; n = 0
        Star r6                 ; i = 0
        Jump test
body:   GetNamed r4, rank
        Star r8
        GetNamed r8, value
        Star r8                 ; c.rank.value
        GetNamed r4, v2
        Star r9
        GetNamed r9, level
        Star r9
        GetNamed r9, value      ; c.v2.level.value
        TestLessThan r8
        JumpIfFalse second
        Ldar r5
        Inc
        Star r5                 ; n++
second: GetNamed r4, v1
        Star r8
        GetNamed r8, level
        Star r8
        GetNamed r8, value
        Star r8                 ; c.v1.level.value
        GetNamed r4, rank
        Star r9
        GetNamed r9, value      ; c.rank.value
        TestLessThan r8
        JumpIfFalse third
        Ldar r5
        AddSmi 2
        Star r5                 ; n += 2
third:  Ldar r6
        BitAndSmi 1023
        TestEqualSmi 0
        JumpIfFalse next
        GetNamed r4, v1
        Star r7                 ; t = c.v1
        GetNamed r4, v2
        SetNamed r4, v1         ; c.v1 = c.v2
        Ldar r7
        SetNamed r4, v2         ; c.v2 = t
next:   Ldar r6
        Inc
        Star r6                 ; i++
test:   Ldar r6
        JumpIfLessSmi 1000000, body
        Ldar r5
        Return
end
