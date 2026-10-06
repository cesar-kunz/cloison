// Overriding method calling the parent's version: X.parent.prototype.m.call(this, ...).
// features: loop, call, object, new, proto, call-method
// expect: 4250000
function Base() { this.marks = 0; }
Base.prototype.mark = function (m) { this.marks += m; };
function Sub() { Sub.parent.call(this); this.extra = 0; }
Sub.parent = Base;
function Tmp() {}
Tmp.prototype = Base.prototype;
Sub.prototype = new Tmp();
Sub.prototype.mark = function (m) {
  Sub.parent.prototype.mark.call(this, m);
  this.extra += 1;
};
function main() {
  var o = new Sub();
  for (var i = 0; i < 500000; i++) o.mark(i & 15);
  return o.marks + o.extra;
}
main();
