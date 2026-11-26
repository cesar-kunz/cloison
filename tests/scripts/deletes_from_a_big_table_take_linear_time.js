// An object used as a table loses its keys in time that doesn't grow with
// the table: every third of 300,000 (each delete moved the slots after
// it down one, and their values: 37 s; V8 95 ms), from the last (a stack:
// each compared the shape's keys with its parent's, 17 s for 200,000),
// from the first (a queue), and a queue that keeps its size while a
// million keys go through it. Quadratic, any of them would time out. The
// keys left, in order, and their values are what V8, SpiderMonkey and
// JavaScriptCore give.
// release only: a debug build takes a minute
// stress slow: 58 s collecting at every safepoint (nightly runs it)
// expect: 200000:-2061899968 k1,k2,k4 210000:-2045108527 k299910,k299940,k299970 -30  1 true false 100:-303319470 k98,k99 100:-1749068046 k199900,k199901 1000:-1631265932 k999000 k999999
var out = [];
function sum(o) {
  var h = 0, n = 0;
  for (var k in o) { h = (h * 31 + k.length + (o[k] | 0)) | 0; n++; }
  return n + ":" + h;
}
function table(n) {
  var o = {};
  for (var i = 0; i < n; i++) o["k" + i] = i;
  return o;
}
var o = table(300000);
for (var i = 0; i < 300000; i += 3) delete o["k" + i];
out.push(sum(o), Object.keys(o).slice(0, 3).join());
for (var i = 0; i < 300000; i += 30) o["k" + i] = -i;
out.push(sum(o), Object.keys(o).slice(-3).join(), o.k30, o.k3, o.k1, "k0" in o, "k3" in o);
o = table(200000);
for (var i = 199999; i >= 100; i--) delete o["k" + i];
out.push(sum(o), Object.keys(o).slice(-2).join());
o = table(200000);
for (var i = 0; i < 199900; i++) delete o["k" + i];
out.push(sum(o), Object.keys(o).slice(0, 2).join());
o = table(1000);
for (var i = 1000; i < 1000000; i++) { o["k" + i] = i; delete o["k" + (i - 1000)]; }
out.push(sum(o), Object.keys(o)[0], Object.keys(o)[999]);
out.join(" ")
