// Pre-increment of a field, returning the new value.
// features: loop, call, object, new, proto
// expect: 1785293664
function P() { this.mark = 0; }
P.prototype.next = function () { return ++this.mark; };
function main() {
  var p = new P(), sum = 0;
  for (var i = 0; i < 1000000; i++) sum = (sum + p.next()) | 0;
  return sum + p.mark;
}
main();
