// `instanceof` through a cache per site, keyed by the receiver's shape
// and the function's `prototype` (a new one, or a function of another
// shape, misses it), and throwing where it should.
// expect: 326439998 1 1

function A() {} function B() {} B.prototype = new A(); function C() {}
var objs = [new A(), new B(), new C(), {}, "str", 5, null, [1], new B()];
function count(xs, F) { var n = 0; for (var i = 0; i < xs.length; i++) if (xs[i] instanceof F) n++; return n; }
var t = 0;
for (var r = 0; r < 20000; r++) {
  t += count(objs, A) * 100 + count(objs, B) * 10 + count(objs, C) + count(objs, Object) * 1000 + count(objs, Array) * 10000;
  if (r == 10000) { C.prototype = A.prototype; }
  if (r == 15000) { var fx = function () {}; fx.extra = 1; t += count(objs, fx); }
}
var err = 0; try { count(objs, 3); } catch (e) { err = 1; }
var err2 = 0; try { var g = function () {}; g.prototype = 7; count([{}], g); } catch (e) { err2 = 1; }
t + " " + err + " " + err2;
