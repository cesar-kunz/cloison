// Getters and setters in object literals.
// for: pdfjs (38 getters)
// expect: 10,50,5,L,150000
var o = {
  _v: 1,
  get v() { return this._v * 10; },
  set v(x) { this._v = x + 1; },
  get lazy() { return "L"; }
};
var r = [o.v];
o.v = 4;
r.push(o.v, o._v, o.lazy);
var n = 0;
for (var i = 0; i < 3000; i++) n += o.v;
r.push(n);
r.join(",");
