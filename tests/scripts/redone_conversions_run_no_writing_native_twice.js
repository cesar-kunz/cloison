// Such a conversion, redone, doesn't run again a native that wrote before
// it got to the bytecode call: an array-like's `shift` as its `toString`
// (or its `join`, or `Symbol.toPrimitive`) shifts once, a getter that
// `pop`s pops once. Nor does it go on to the next method, which may throw:
// `pop` as `valueOf` before a `toString` that throws for objects; and
// what the other operand's conversion throws meanwhile is thrown only if
// it throws again after the redo. The expected value is SpiderMonkey's,
// JSC's and QuickJS's.
// expect: 1,2,1,2,1,2,Error: 6,1,7,0,TypeError,0,15000

var r = [];
function mk(k, m) {
  var o = { 0: 1, 1: 2, length: 3 };
  o[k] = m;
  Object.defineProperty(o, 2, { get: function () { return 3; }, configurable: true });
  return o;
}
var o = mk("toString", Array.prototype.shift);
r.push(o + "", o.length);
var a = mk("join", Array.prototype.shift);
a.toString = Array.prototype.toString;
r.push(a + "", a.length);
var s = mk(Symbol.toPrimitive, Array.prototype.shift);
r.push(s + "", s.length);
var q = [5, 6], e = new Error("x");
Object.defineProperty(e, "message", { get: Array.prototype.pop.bind(q) });
r.push(e + "", q.length);
var p = { 0: 7, length: 1, valueOf: Array.prototype.pop, toString: Number.prototype.toString };
r.push(+p, p.length);
var b = { valueOf: Number.prototype.toString };
p[0] = 8, p.length = 1;
try { r.push(p + b); } catch (x) { r.push(x.constructor.name, p.length); }
function f(n) { var t = 0; for (var i = 0; i < n; i++) { var x = mk("toString", Array.prototype.shift); t += +(x + "") + x.length; } return t; }
r.push(f(5000));
r.join(",");
