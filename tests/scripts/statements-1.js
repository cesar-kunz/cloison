// `while`, `do … while`, `break` and `continue`, labeled ones too.
// A loop whose first test fails; one with a computed bound; an endless
// one left by `return`.
// A labeled block; `if`/`else` chains; an early return inside a loop.
// expect: 38368
function f() {
               var n = 0, i = 0;
               while (i < 100) { i++; if (i % 3 == 0) continue; n += i; }
               do { n++; } while (n < 3000);
               outer: for (var a = 0; a < 10; a++)
                 for (var b = 0; b < 10; b++) { if (b == 5) continue outer; if (a == 7) break outer; n += 1000; }
               return n;
             }
             f();
