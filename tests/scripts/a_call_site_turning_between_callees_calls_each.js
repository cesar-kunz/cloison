// A call site turning between two to eight callees, of other code and of
// one code's closures, some made per call, and whose callees are replaced
// midway: each call runs the callee it was made with, whichever of the
// site's ways holds it. Expected value from V8.
// expect: 8400 44850 59900 78750 53700 120600 135111 117267 2985150 1400

var out = [];
class Shape { constructor(x) { this.x = x; } area() { return this.x; } }
class Sq extends Shape { area() { return this.x * this.x; } }
class Ci extends Shape { area() { return (this.x * this.x * 3) | 0; } }
var shapes = [new Shape(2), new Sq(3), new Ci(4), new Sq(5)];
var s = 0;
for (var i = 0; i < 400; i++) s = (s + shapes[i & 3].area()) | 0;
out.push(s);
function mk(k) { return function (x) { return x * k; }; }
var fs = [function (x) { return x + 1; }, function (x) { return x - 1; }, mk(2), mk(3), function (x) { return -x; },
  function (x) { return x * 10; }, mk(5), function (x) { "use strict"; return this === undefined ? x : -1; }];
for (var n = 2; n <= fs.length; n++) {
  var t = 0;
  for (var i = 0; i < 300; i++) t = (t + fs[i % n](i)) | 0;
  out.push(t);
}
var t = 0;
for (var i = 0; i < 300; i++) { var g = i % 3 == 0 ? mk(i) : fs[i % 2]; t = (t + g(i)) | 0; }
out.push(t);
var o = { m: function () { return 1; } }, p = { m: function () { return 2; } }, q = { m: function () { return 3; } };
var objs = [o, p, q], u = 0;
for (var i = 0; i < 300; i++) {
  if (i == 150) { p.m = function () { return 20; }; q.m = o.m; }
  u += objs[i % 3].m();
}
out.push(u);
out.join(" ");
