// A global reassigned between phases; methods keep reading it.
// features: loop, call, object, new, proto, global, null
// expect: 1500000
var current = null;
function P(k) { this.k = k; this.n = 0; }
P.prototype.tick = function () { this.n += this.k; };
function phase(k) {
  current = new P(k);
  for (var i = 0; i < 100000; i++) current.tick();
  return current.n;
}
function main() {
  var sum = 0;
  for (var r = 0; r < 5; r++) sum += phase(1) + phase(2);
  return sum;
}
main();
