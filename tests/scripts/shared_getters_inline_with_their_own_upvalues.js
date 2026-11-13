// A getter shared by several accessors (one closure each, of one code),
// inlined after a constructor call: its frame reads its upvalues from the
// getter the receiver's shape proves, not from a frame header nothing
// wrote (the constructor's, there: wrong upvalues, a crash). The expected
// value is V8's.
// expect: A1 B2

var reg = new WeakMap();
var tag = "ctor";
var Ctor = (function () { var up = "ctor-up"; return function Ctor(v) { reg.set(this, { v: v, getA: function () { return "A" + this.v; }, getB: function () { return "B" + this.v; } }); this.t = up + tag; }; })();
function accessor(getter) { return { get: function () { return reg.get(this)[getter](); }, configurable: true }; }
Object.defineProperty(Ctor.prototype, "a", accessor("getA"));
Object.defineProperty(Ctor.prototype, "b", accessor("getB"));
function f(u) { var o = new Ctor(u); return o.a; }
function g(u) { var o = new Ctor(u); return o.b; }
for (var i = 0; i < 30000; i++) { f(i); g(i); }
[f(1), g(2)].join(" ");
