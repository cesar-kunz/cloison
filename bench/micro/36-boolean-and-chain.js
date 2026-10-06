// && chains over boolean fields of three objects, result stored in a field.
// features: loop, object, new
// expect: 250000
function V(s) { this.fixed = s; }
function main() {
  var a = new V(true), b = new V(true), c = new V(true), out = new V(false), n = 0;
  for (var i = 0; i < 1000000; i++) {
    a.fixed = (i & 1) == 0;
    b.fixed = (i & 2) == 0;
    out.fixed = a.fixed && b.fixed && c.fixed;
    if (out.fixed) n++;
  }
  return n;
}
main();
