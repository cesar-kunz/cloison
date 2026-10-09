// try/finally: on normal exit, on return, on throw, on break.
// for: earley-boyer, typescript, zlib
// expect: r,e,,01ftfFx
var log = "";
function f(x) {
  try { if (x) return "r"; log += "t"; } finally { log += "f"; }
  return "e";
}
function g() {
  try { try { throw "x"; } finally { log += "F"; } } catch (e) { log += e; }
}
for (var i = 0; i < 3; i++) { try { if (i == 1) break; } finally { log += i; } }
[f(true), f(false), g(), log].join(",");
