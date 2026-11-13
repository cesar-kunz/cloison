// An object key in compiled code's `o[k]`, `k in o` and `delete o[k]` is
// converted as the interpreter converts it: its `toString` (or
// `Symbol.toPrimitive`) called each time, what it throws thrown.
// expect: 5000 5000 5000 true false 20000 5000 boom

var calls = 0, key = { toString() { calls++; return "k"; } };
var tgt = { k: 1, "[object Object]": 99 };
function g(n) { var s = 0; for (var i = 0; i < n; i++) s += tgt[key]; return s; }
function h(n) { var s = 0; for (var i = 0; i < n; i++) s += (key in { k: 1 }) ? 1 : 0; return s; }
function d(n) { var t = {}, r; for (var i = 0; i < n; i++) { t.k = 1; r = delete t[key]; } return r + " " + ("k" in t); }
var sp = { [Symbol.toPrimitive]() { return "k"; } };
function g2(n) { var s = 0; for (var i = 0; i < n; i++) s += tgt[sp]; return s; }
var bad = { toString() { throw new Error("boom"); } };
function e(n, k) { var s = 0; for (var i = 0; i < n; i++) { try { s += tgt[i == n - 1 ? k : "k"]; } catch (x) { s = x.message; } } return s; }
for (var j = 0; j < 5; j++) e(2000, "k");
[g(5000), g(5000), h(5000), d(5000), calls, g2(5000), e(2000, bad)].join(" ");
