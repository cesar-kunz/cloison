// `this(a, b)` compiled, its callee (making `arguments`, which its
// upvalue keeps from compiling) inlined, exits at its start: the
// interpreter's `arguments` reads them in the caller's frame, where
// the `Mov`s to the call's registers, renamed away, wrote nothing (it
// found what the stack held; sass's `Function.prototype.call$2`). As
// V8, SpiderMonkey, JavaScriptCore and QuickJS.
// expect: 180021.5

function wrap(call, f2) { return function () { return call(f2, Array.prototype.slice.apply(arguments)); }; }
function callIt(f, args) { return f(args[0], args[1]); }
function val(x) { return typeof x == "number" ? x : typeof x == "string" ? x.length : x == null ? 0 : x.v === undefined ? 1 : x.v; }
var fs = [];
for (var k = 0; k < 8; k++) {
  var w = wrap(callIt, (function (k) { return function (x, y) { return val(x) * k + val(y); }; })(k));
  w.c = new Function("a", "b", "return this(a, b);" + " ".repeat(k));
  fs.push(w);
}
var vals = [1, 2.5, "abc", null, { v: 3 }, { w: 1, v: 4 }, [5], { u: 2, w: 1, v: 6 }, true, undefined, { x: 1 }, "z"];
function drive(n) {
  var s = 0;
  for (var i = 0; i < n; i++) { var junk = { a: i, b: [i, i] }; s += fs[i & 7].c(vals[i % 12], vals[(i >> 3) % 12]); }
  return s;
}
drive(20000)
