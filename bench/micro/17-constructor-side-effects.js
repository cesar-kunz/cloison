// `new X(...)` whose result is discarded: the constructor registers the object elsewhere.
// features: loop, object, new, proto, array
// expect: 200
function Node() { this.links = []; }
function Link(a, b) { this.a = a; this.b = b; a.links.push(this); b.links.push(this); }
function main() {
  var first = new Node(), prev = first, total = 0;
  for (var r = 0; r < 100; r++) {
    for (var i = 0; i < 1000; i++) {
      var n = new Node();
      new Link(prev, n);
      prev = n;
    }
    total += first.links.length + prev.links.length;
    first = new Node();
    prev = first;
  }
  return total;
}
main();
