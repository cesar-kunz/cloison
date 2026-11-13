// An operator converting an object whose `toString` is a native that
// calls a bytecode function (an array's own `join`, an error's `message`
// getter, a RegExp's `source` getter) redoes where the call can be made,
// in the interpreter and in compiled code (#75). The expected value is
// SpiderMonkey's, JSC's and QuickJS's.
// expect: J,Error: m,/s/,true,true,10,-5,5,42,boom,40000,40000,30004

var joins = 0, a = [];
a.join = function () { joins++; return "J"; };
var n = [1];
n.join = function () { return "5"; };
var e = new Error("x");
Object.defineProperty(e, "message", { get: function () { return "m"; } });
var re = /q/;
Object.defineProperty(re, "source", { get: function () { return "s"; } });
var t = [];
t.join = function () { throw new Error("boom"); };
var o = { J: 42 }, r = [a + "", e + "", re + "", a == "J", a < "K", n * 2, -n, n | 0, o[a]];
try { t + ""; } catch (x) { r.push(x.message); }
function f(k) { var s = 0; for (var i = 0; i < k; i++) s += (a + "").length + n * 1 + (o[a] == 42) + (a == "J"); return s; }
r.push(f(5000), f(5000), joins);
r.join(",");
