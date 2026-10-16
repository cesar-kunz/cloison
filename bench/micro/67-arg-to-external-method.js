// Internal objects passed as arguments to a method of a global object that never stores them.
// cloison: not an escape; the arguments fixed internal.
// features: loop, call, object, new, proto, global
// expect: 7050000
function Service() { this.mark = 0; }
Service.prototype.weigh = function (a, b) { this.mark++; return a.k * b.k; };
var service = new Service();
function Item(k) { this.k = k; }
function main() {
  var sum = 0;
  for (var i = 0; i < 300000; i++) {
    var a = new Item(i & 15), b = new Item(3);
    sum += service.weigh(a, b);
  }
  return sum + service.mark;
}
main();
