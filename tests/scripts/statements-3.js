// `while`, `do … while`, `break` and `continue`, labeled ones too.
// A loop whose first test fails; one with a computed bound; an endless
// one left by `return`.
// A labeled block; `if`/`else` chains; an early return inside a loop.
// expect: 13
function f(x) {
               b: { if (x == 1) break b; return 10; }
               for (var i = 0; i < 10; i++) if (i == x + 2) return i; else if (i == 9) return -1;
             }
             function g() { return f(1) + f(2); }
             g();
