// Subclass constructors running the parent constructor via Function.prototype.call.
// features: loop, call, object, new, proto, call-method
// expect: 1800000
function Base(s) { this.s = s; }
function Mid(a, s) { Mid.parent.call(this, s); this.a = a; }
Mid.parent = Base;
function Leaf(a, b, s) { Leaf.parent.call(this, a, s); this.b = b; }
Leaf.parent = Mid;
function main() {
  var sum = 0;
  for (var i = 0; i < 300000; i++) {
    var o = new Leaf(i & 7, i & 3, 1);
    sum += o.s + o.a + o.b;
  }
  return sum;
}
main();
