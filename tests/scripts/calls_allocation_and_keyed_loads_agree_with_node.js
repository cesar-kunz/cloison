// The paths the interpreter's call, allocation, keyed-load and number
// changes take, and their fallbacks: one keyed load site over arrays of
// every element type, holes, strings, typed arrays, frozen arrays, plain
// objects and both kinds of `arguments`; arithmetic and its stored forms
// on int32s at their bounds, doubles, -0, NaN, strings, objects and
// BigInts; `new` of one constructor's code for several prototypes
// (Prototype's `Class.create`), and after `prototype` is replaced;
// object literals, also with `__proto__`; closures' prototype objects;
// calls with a primitive `this` (sloppy and strict), a full stack, a
// first call of functions made in a loop, a closure made per call,
// several callees at one site and derived classes. The expected value is
// Node's.
// expect: 10 8 4 108 0abcd 0wxyz 26 3.75 30 10 10 19 10 8 4 108 0abcd 0wxyz 26 3.75 30 10 10 19 10 8 4 108 0abcd 0wxyz 26 3.75 30 10 10 19 {"a":1} proto7 {"b":1} proto7 {"c":1,"d":2} proto7 1.5 proto7 2 proto7 {"a":1} proto7 {"b":1} proto7 {"c":1,"d":2} proto7 1.5 proto7 2 proto7 {"a":1} proto7 {"b":1} proto7 {"c":1,"d":2} proto7 1.5 proto7 2 proto7 {"a":1} proto7 {"b":1} proto7 {"c":1,"d":2} proto7 1.5 proto7 2 proto7 3,-1,2,0.5,2,3 3.5,-0.5,3,0.75,3,3.5 3.5,0.5,3,1.3333333333333333,3,3.5 0.30000000000000004,-0.1,0.020000000000000004,0.5,0.020000000000000004,0.30000000000000004 0,-0,-0,NaN,-0,0 0,0,-0,NaN,-0,0 -1,-1,-0,-Infinity,-0,-1 Infinity,0,Infinity,1,Infinity,Infinity 2147483648,2147483646,2147483647,2147483647,2147483647,2147483648 -2147483649,-2147483647,2147483648,2147483648,2147483648,-2147483649 NaN,NaN,NaN,NaN,NaN,NaN NaN,Infinity,-Infinity,NaN,-Infinity,NaN 34,-1,12,0.75,12,34 43,1,12,1.3333333333333333,12,43 2.5,-2.5,0,0,0,2.5 1.5,0.5,0.5,2,0.5,1.5 4.5,0.5,5,1.25,5,4.5 3n,-1n,2n,0n,2n,3n 3,-1,2,0.5,2,3 3.5,-0.5,3,0.75,3,3.5 3.5,0.5,3,1.3333333333333333,3,3.5 0.30000000000000004,-0.1,0.020000000000000004,0.5,0.020000000000000004,0.30000000000000004 0,-0,-0,NaN,-0,0 0,0,-0,NaN,-0,0 -1,-1,-0,-Infinity,-0,-1 Infinity,0,Infinity,1,Infinity,Infinity 2147483648,2147483646,2147483647,2147483647,2147483647,2147483648 -2147483649,-2147483647,2147483648,2147483648,2147483648,-2147483649 NaN,NaN,NaN,NaN,NaN,NaN NaN,Infinity,-Infinity,NaN,-Infinity,NaN 34,-1,12,0.75,12,34 43,1,12,1.3333333333333333,12,43 2.5,-2.5,0,0,0,2.5 1.5,0.5,0.5,2,0.5,1.5 4.5,0.5,5,1.25,5,4.5 3n,-1n,2n,0n,2n,3n 3,-1,2,0.5,2,3 3.5,-0.5,3,0.75,3,3.5 3.5,0.5,3,1.3333333333333333,3,3.5 0.30000000000000004,-0.1,0.020000000000000004,0.5,0.020000000000000004,0.30000000000000004 0,-0,-0,NaN,-0,0 0,0,-0,NaN,-0,0 -1,-1,-0,-Infinity,-0,-1 Infinity,0,Infinity,1,Infinity,Infinity 2147483648,2147483646,2147483647,2147483647,2147483647,2147483648 -2147483649,-2147483647,2147483648,2147483648,2147483648,-2147483649 NaN,NaN,NaN,NaN,NaN,NaN NaN,Infinity,-Infinity,NaN,-Infinity,NaN 34,-1,12,0.75,12,34 43,1,12,1.3333333333333333,12,43 2.5,-2.5,0,0,0,2.5 1.5,0.5,0.5,2,0.5,1.5 4.5,0.5,5,1.25,5,4.5 3n,-1n,2n,0n,2n,3n 46350 xy x z true 134850 {"a":3,"b":1} true 45050 20300 true false true undefined false object number true object object object object object number true object object object object object number true object object object object RangeError 1000 RangeError 59900 0 20 40 60 80 40200 19263822890277134000
var out = [];
function S(v) { return typeof v === "bigint" ? v + "n" : Object.is(v, -0) ? "-0" : String(v); }
function at(a, i) { return a[i]; }
function sumAt(a) { var s = 0; for (var i = 0; i < 4; i++) { var v = at(a, i); s += typeof v === "object" && v ? (v.k || 1) : v === undefined ? 100 : v; } return s; }
Object.prototype[7] = "proto7";
var arrs = [[1, 2, 3, 4], [0.5, 1.5, 2.5, 3.5], [{ k: 1 }, { k: 2, j: 1 }, { m: 1 }, null], [1, , 3, 4], ["a", "b", "c", "d"],
  "wxyz", new Int8Array([5, 6, 7, 8]), new Float64Array([0.25, 0.5, 1, 2]), Object.freeze([9, 8, 7, 6]), { 0: 1, 1: 2, 2: 3, 3: 4 }];
(function () { arrs.push(arguments); })(4, 3, 2, 1);
(function (a, b) { a = 10; arrs.push(arguments); })(1, 2, 3, 4);
for (var r = 0; r < 3; r++) for (var i = 0; i < arrs.length; i++) out.push(S(sumAt(arrs[i])));
var mixed = [[{ a: 1 }], [{ b: 1 }], [{ c: 1, d: 2 }], [1.5], [2]];
for (var r = 0; r < 4; r++) for (var i = 0; i < mixed.length; i++) out.push(JSON.stringify(at(mixed[i], 0)), S(at(mixed[i], 7)));
function arith(a, b) {
  var x = a + b, y = a - b, z = a * b, w = a / b, t;
  t = a * b; var u = t;
  var p = a + b; var q = p;
  return [x, y, z, w, u, q].map(S).join(",");
}
var nums = [[1, 2], [1.5, 2], [2, 1.5], [0.1, 0.2], [-0, 0], [0, -0], [-1, 0], [1e308, 1e308], [2147483647, 1], [-2147483648, -1],
  [NaN, 1], [Infinity, -Infinity], ["3", 4], [4, "3"], [null, 2.5], [true, 0.5], [{ valueOf: function () { return 2.5; } }, 2], [1n, 2n]];
for (var r = 0; r < 3; r++) for (var i = 0; i < nums.length; i++) { try { out.push(arith(nums[i][0], nums[i][1])); } catch (e) { out.push(e.constructor.name); } }
var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var A = Class.create(), B = Class.create(), C = Class.create();
A.prototype = { initialize: function (x) { this.x = x; } };
B.prototype = { initialize: function (x, y) { this.x = x; this.y = y; } };
C.prototype = { initialize: function () { this.n = arguments.length; } };
var s = 0;
for (var i = 0; i < 300; i++) { var a = new A(i), b = new B(i, 1), c = new C(1, 2, 3); s += a.x + b.y + c.n + (a instanceof A ? 1 : 0) + (b instanceof A ? 100 : 0); }
out.push(S(s), Object.keys(new B(1, 2)).join(""), Object.keys(new A(1)).join(""));
B.prototype = { initialize: function (x) { this.z = x; } };
out.push(Object.keys(new B(5)).join(""), S(new B(5) instanceof B));
function lit(i) { return { a: i, b: 1 }; }
function lit0() { return {}; }
function litp(i) { return { __proto__: { p: i }, a: i }; }
var t = 0;
for (var i = 0; i < 300; i++) { var o = lit(i), e = lit0(), q = litp(i); t += o.a + o.b + Object.keys(e).length + q.p + q.a; if (i == 150) { e.z = 1; o.c = 2; } }
out.push(S(t), JSON.stringify(lit(3)), JSON.stringify(Object.getPrototypeOf(lit0()) === Object.prototype));
function P(x) { this.x = x; }
var u = 0;
for (var i = 0; i < 300; i++) { var p = new P(i); u += p.x; if (i == 100) P.prototype.y = 1; if (p.y) u++; }
out.push(S(u));
function mk(i) { var f = function () { return i; }; return f; }
var v = 0, fs = [];
for (var i = 0; i < 200; i++) { var f = mk(i); v += f() + (f.prototype.constructor === f ? 1 : 0) + Object.getOwnPropertyNames(f.prototype).length; if (i % 50 == 0) fs.push(f); }
out.push(S(v), S(new fs[1]() instanceof fs[1]), S(fs[0].prototype === fs[1].prototype), S(fs[2].hasOwnProperty("prototype")));
var arrow = () => 1;
out.push(S(arrow.prototype), S("prototype" in arrow));
String.prototype.kind = function () { return typeof this; };
Number.prototype.kind = function () { "use strict"; return typeof this; };
Boolean.prototype.kind = function () { return this instanceof Boolean; };
function sloppyThis() { return this === undefined ? "u" : typeof this; }
var holder = { f: sloppyThis };
for (var r = 0; r < 3; r++) out.push("x".kind(), (5).kind(), true.kind(), holder.f(), sloppyThis.call(null), sloppyThis.call(7), sloppyThis());
function deep(n) { return n == 0 ? 0 : 1 + deep(n - 1); }
function tryDeep() { try { return deep(1e7); } catch (e) { return e.constructor.name; } }
out.push(tryDeep(), S(deep(1000)), tryDeep());
var calls = 0;
for (var i = 0; i < 200; i++) { var g = function (x) { return x + 1; }; calls += g(i); var h = new Function("x", "return x * 2"); calls += h(i); }
out.push(S(calls));
var cs = [function (x) { return x; }, function (x) { return -x; }, function (x) { return x * 2; }, function (x) { return x + 0.5; }, function (x) { return "" + x; }];
var w = "";
for (var i = 0; i < 100; i++) { w = cs[i % 5](i) + ""; if (i % 20 == 0) out.push(w); }
class Base { constructor(x) { this.x = x; } }
class Derived extends Base { constructor(x) { super(x); this.y = x + 1; } }
var d = 0;
for (var i = 0; i < 200; i++) { var o = new Derived(i); d += o.x + o.y + (o instanceof Base ? 1 : 0); }
out.push(S(d));
function fact(n) { return n <= 1 ? 1 : n * fact(n - 1); }
var fsum = 0;
for (var i = 0; i < 3000; i++) fsum += fact(i % 20);
out.push(S(fsum));
out.join(" ");
