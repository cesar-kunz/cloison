// Field loads at a site seeing two shapes that store the field at different offsets.
// features: loop, object, new, array
// expect: 50000000
function Eq(a, b) { this.s = 0; this.a = a; this.b = b; this.dir = 1; }
function Sc(a, b) { this.dir = -1; this.k = 10; this.o = 5; this.s = 0; this.a = a; this.b = b; }
function main() {
  var cs = [];
  for (var i = 0; i < 100; i++) cs.push((i & 1) ? new Eq(i, i + 1) : new Sc(i, i + 1));
  var sum = 0;
  for (var r = 0; r < 10000; r++) {
    for (var i = 0; i < 100; i++) {
      var c = cs[i];
      sum += c.dir == 1 ? c.b : c.a;
    }
  }
  return sum;
}
main();
