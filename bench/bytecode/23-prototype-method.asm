; Expected bytecode for bench/micro/23-prototype-method.js.
;
;   function Counter() { this.n = 0; }
;   Counter.prototype.bump = function (k) { this.n += k; };
;   Counter.prototype.get = function () { return this.n; };
;   function main() {
;     var c = new Counter();
;     for (var i = 0; i < 1000000; i++) c.bump(i & 3);
;     return c.get();
;   }
;   main();

script                          ; r1 temporary, r2 unused argument window
        CreateClosure Counter
        StaGlobal Counter
        CreateClosure main
        StaGlobal main
        LdaGlobal Counter
        Star r1
        GetNamed r1, prototype
        Star r1
        CreateClosure bump
        SetNamed r1, bump       ; Counter.prototype.bump = function (k) {…}
        LdaGlobal Counter
        Star r1
        GetNamed r1, prototype
        Star r1
        CreateClosure get
        SetNamed r1, get        ; Counter.prototype.get = function () {…}
        LdaGlobal main
        Star r1
        CallUndefined r1, r2, 0
        Return
end

function Counter()              ; r0 this
        LdaSmi 0
        SetNamed r0, n          ; this.n = 0
        LdaUndefined
        Return
end

function bump(k)                ; r0 this, r1 k, r2 temporary
        GetNamed r0, n
        Star r2
        Ldar r1
        Add r2
        SetNamed r0, n          ; this.n += k
        LdaUndefined
        Return
end

function get()                  ; r0 this
        GetNamed r0, n
        Return
end

function main()                 ; r1 c, r2 i, r3 callee, r4 this, r5 argument
        LdaGlobal Counter
        Star r3
        Construct r3, r4, 0
        Star r1                 ; c = new Counter()
        LdaSmi 0
        Star r2                 ; i = 0
        Jump test
body:   GetNamed r1, bump
        Star r3
        Mov r1, r4
        Ldar r2
        BitAndSmi 3
        Star r5
        Call r3, r4, 1          ; c.bump(i & 3)
        Ldar r2
        Inc
        Star r2                 ; i++
test:   Ldar r2
        JumpIfLessSmi 1000000, body
        GetNamed r1, get
        Star r3
        Mov r1, r4
        Call r3, r4, 0          ; c.get()
        Return
end
