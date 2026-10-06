// Method found two levels up the prototype chain.
// features: loop, call, object, new, proto, call-method
// expect: 3000000
function inherit(child, parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  child.prototype = new Tmp();
  child.parent = parent;
}
function A(s) { this.s = s; }
A.prototype.weight = function () { return this.s + this.bonus(); };
function B(s) { B.parent.call(this, s); }
inherit(B, A);
B.prototype.bonus = function () { return 1; };
function C(s) { C.parent.call(this, s); }
inherit(C, B);
function main() {
  var c = new C(2), sum = 0;
  for (var i = 0; i < 1000000; i++) sum += c.weight();
  return sum;
}
main();
