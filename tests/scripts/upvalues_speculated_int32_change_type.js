// Upvalues that held int32s when compiled are loaded as int32s, checked
// (exiting when one changed: then compiled as any value); a double
// compared with `>`, `<=`, `>=` compares without the runtime, NaN
// included. The expected value is Node's.
// expect: 6175,20,0,3418.75,156.75,8,363,11565,60

function Grid(n) {
  var width = n, scale = 2, s = 0;
  function sum(a) { var t = 0; for (var i = 0; i < width; i++) t += a[i] * scale + width; return t; }
  function less(x) { var c = 0; for (var i = 0; i < width; i++) if (x > i + 0.5) c++; return c; }
  return {
    sum: sum, less: less,
    setScale: function (v) { scale = v; }, setWidth: function (v) { width = v; }
  };
}
var g = Grid(50), a = [];
for (var i = 0; i < 60; i++) a.push(i * 1.5);
var r = [];
for (var k = 0; k < 300; k++) s0 = g.sum(a);
r.push(g.sum(a), g.less(20.25), g.less(NaN));
g.setScale(0.5);
r.push(g.sum(a));
g.setWidth(10.5);
r.push(g.sum(a), g.less(7.75));
g.setScale("3");
r.push(g.sum(a));
g.setWidth(60);
for (var k = 0; k < 300; k++) s0 = g.sum(a);
r.push(g.sum(a), g.less(1e9));
r.join(",");
