// A compiled function called directly that throws from a runtime call
// closes its frame's upvalues: its closures kept pointing into the
// stack past it, read what later calls left there, and an unwind
// further down closed them from that (cloison-fuzz BUGS.md #20: a
// segmentation fault in the write barrier).
// expect: 2999000 0

var kept = [];
function g(p) { kept.push(() => p); decodeURIComponent(p % 3 == 0 ? "%" : "a"); return p; }
function caller(n) { var s = 0; for (var i = 0; i < n; i++) { try { s += g(i); } catch (e) { s -= 1; } } return s; }
function clobber(a, b, c, d, e, f) { return a + b + c + d + e + f; }
function outer(n) { var s = 0; for (var i = 0; i < n; i++) s += clobber(i, 1.5, 2.5, 3.5, 4.5, 5.5); return s; }
var s = caller(3000);
outer(3000);
var bad = 0;
for (var i = 0; i < kept.length; i++) if (kept[i]() !== i) bad++;
s + " " + bad;
