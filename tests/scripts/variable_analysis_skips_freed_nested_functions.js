// The JIT's analysis of a variable walks the functions nested in the one
// declaring it, which the analysis pins; once that function is dead, the
// nested functions it didn't pin are freed, and its list of them must not
// lead the next analysis to them (Codex on #13: compiling `b` after `c`
// was freed read `c`'s freed code). Fifty such functions, each from its
// own eval, so that no stale word keeps all of them alive.
// expect: 25500000

var keep = [];
function hot(f) { var s = 0; for (var i = 0; i < 20000; i++) s += f(); return s; }
function make(k) {
  return (0, eval)("(function () {" +
    "var outer = function () {" +
    "  let o = {x: " + k + "};" +
    "  function a() { return o.x; }" +
    "  function b() { return o.x + 1; }" +
    "  function c() { return " + k + "; }" +
    "  return [a, b, c];" +
    "};" +
    "var fs = outer(); hot(fs[0]); return fs[1]; })()");
}
for (var k = 0; k < 50; k++) keep.push(make(k));
function clobber(n) { var a = 0, b = 0, c = 0, d = 0; return n ? clobber(n - 1) + a + b + c + d : 0; }
clobber(2000);
for (var r = 0; r < 4; r++) { var junk = []; for (var i = 0; i < 100000; i++) junk.push({i}); $262.gc(); }
var t = 0;
for (var k = 0; k < 50; k++) t += hot(keep[k]);
t
