// Methods comparing a field against a global constant property, called in a hot loop.
// features: loop, call, object, new, proto, global
// expect: 500000
var Dir = new Object();
Dir.NONE = 0;
Dir.FORWARD = 1;
function Bin(a, b, d) { this.a = a; this.b = b; this.dir = d; }
Bin.prototype.input = function () { return (this.dir == Dir.FORWARD) ? this.a : this.b; };
Bin.prototype.output = function () { return (this.dir == Dir.FORWARD) ? this.b : this.a; };
Bin.prototype.ok = function () { return this.dir != Dir.NONE; };
function main() {
  var c = new Bin(3, 4, 1), sum = 0;
  for (var i = 0; i < 500000; i++) {
    c.dir = i & 1;
    if (c.ok()) sum += c.output() - c.input();
    else sum += 1;
  }
  return sum;
}
main();
