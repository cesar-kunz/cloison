// One inherited method running on receivers of two subclasses with different field orders.
// features: loop, call, object, new, proto, call-method, array
// expect: 98510000
function inherit(child, parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  child.prototype = new Tmp();
  child.parent = parent;
}
function Bin(a, b) { this.a = a; this.b = b; this.dir = 1; }
Bin.prototype.output = function () { return this.dir == 1 ? this.b : this.a; };
function Eq(a, b) { Eq.parent.call(this, a, b); }
inherit(Eq, Bin);
function Sc(a, b, k) { this.dir = -1; this.k = k; Sc.parent.call(this, a, b); }
inherit(Sc, Bin);
function main() {
  var cs = [];
  for (var i = 0; i < 100; i++) cs.push((i & 1) ? new Eq(i, 2 * i) : new Sc(i, 3 * i, 7));
  for (var i = 0; i < 100; i++) cs[i].dir = (i % 3 == 0) ? -1 : 1;
  var sum = 0;
  for (var r = 0; r < 10000; r++) for (var i = 0; i < 100; i++) sum += cs[i].output();
  return sum;
}
main();
