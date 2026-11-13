// A native that compiled code's runtime calls run (a getter, as
// `String.prototype.trim`, by name, key or index; a proxy's `get`, `has`
// or `deleteProperty` trap; a `Symbol.hasInstance`) and that converts
// with a bytecode `toString` has the instruction redone in the
// interpreter, not going on with `undefined` (#129); what that `toString`
// throws is thrown once. The redo runs no writing native twice: a
// `reverse` as `toString`, before a bytecode `valueOf`, reverses once a
// read. The expected value is SpiderMonkey's and JSC's.
// expect: ab,ab,ab,ab,,false,false,false,false,false,20000,boom,3000,v,5000,-2

var calls = 0, o = { toString() { calls++; return " ab "; } };
Object.defineProperty(o, "t", { get: String.prototype.trim });
Object.defineProperty(o, 3, { get: String.prototype.trim });
var sp = function () { return " "; };
var p = new Proxy({}, { toString: sp, get: String.prototype.trim, has: String.prototype.trim, deleteProperty: String.prototype.trim });
var c = { [Symbol.hasInstance]: String.prototype.trim, toString: sp };
var n = 0, th = { toString() { if (++n == 3000) throw new Error("boom"); return "x"; } };
Object.defineProperty(th, "t", { get: String.prototype.trim });
var w = { 0: 1, 1: 2, length: 2, toString: Array.prototype.reverse, valueOf() { return " v "; } };
Object.defineProperty(w, "t", { get: String.prototype.trim });
function loop(g) { var v; for (var i = 0; i < 5000; i++) v = g(i); return v; }
var r = [
  loop(function () { return o.t; }),
  loop(function () { var k = "t"; return o[k]; }),
  loop(function () { return o[3]; }),
  loop(function (i) { return o[i - i + 3]; }),
  loop(function () { return p.x; }),
  loop(function () { return "x" in p; }),
  loop(function () { var k = "x"; return k in p; }),
  loop(function () { return delete p.x; }),
  loop(function () { var k = "x"; return delete p[k]; }),
  loop(function () { return {} instanceof c; }),
  calls,
];
try { loop(function () { return th.t; }); } catch (e) { r.push(e.message, n); }
function rev(w) { var v, s = ""; for (var i = 0; i < 5000; i++) { v = w.t; s += w[0]; } return [v, s.length, s.indexOf("11") + s.indexOf("22")]; }
r.push(rev(w));
r.join();
