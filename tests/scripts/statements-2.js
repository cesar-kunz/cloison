// `while`, `do … while`, `break` and `continue`, labeled ones too.
// A loop whose first test fails; one with a computed bound; an endless
// one left by `return`.
// A labeled block; `if`/`else` chains; an early return inside a loop.
// expect: 11
function f(k) {
               var n = 0;
               for (var i = 5; i < 3; i++) n += 100;
               for (var a = 9, b = 0; a < 3; a++) n += 100;
               for (var j = 0; j < k; j++) n++;
               while (true) { n += 2; if (n == 11) return n; }
             }
             f(5);
