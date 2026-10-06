// Monomorphic method call through the prototype.
// features: loop, call, object, new, proto
// expect: 1500000
function Counter() { this.n = 0; }
Counter.prototype.bump = function (k) { this.n += k; };
Counter.prototype.get = function () { return this.n; };
function main() {
  var c = new Counter();
  for (var i = 0; i < 1000000; i++) c.bump(i & 3);
  return c.get();
}
main();
