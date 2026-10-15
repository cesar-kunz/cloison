// Forwarding through Function.prototype.call and apply to a method.
// features: loop, object, new, proto, call, call-method, array
// expect: 228275296
function Acc() { this.total = 0; }
Acc.prototype.addPair = function (a, b) { this.total += a - b; return this.total; };
function viaCall(o, a, b) { return Acc.prototype.addPair.call(o, a, b); }
function viaApply(o, args) { return Acc.prototype.addPair.apply(o, args); }
function main() {
  var o = new Acc(), args = [3, 1], s = 0;
  for (var i = 0; i < 500000; i++) s = (s + viaCall(o, i & 15, 2) + viaApply(o, args)) & 0xfffffff;
  return s;
}
main();
