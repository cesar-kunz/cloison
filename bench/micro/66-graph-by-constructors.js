// A chain of nodes and links built entirely by constructor side effects, then walked.
// toi: everything internal: no barriers on these stores, freed with the region.
// features: loop, call, object, new, proto, array, null
// expect: 1515000
function Node(i) { this.value = i; this.links = []; }
Node.prototype.attach = function (l) { this.links.push(l); };
function Link(a, b) { this.a = a; this.b = b; this.register(); }
Link.prototype.register = function () { this.a.attach(this); this.b.attach(this); };
function build(n) {
  var prev = null, first = null;
  for (var i = 0; i <= n; i++) {
    var v = new Node(i);
    if (prev != null) new Link(prev, v);
    if (i == 0) first = v;
    prev = v;
  }
  return first;
}
function walk(first) {
  var n = first, sum = 0;
  while (true) {
    sum += n.value;
    var next = null;
    for (var i = 0; i < n.links.length; i++) if (n.links[i].a == n) next = n.links[i].b;
    if (next == null) return sum;
    n = next;
  }
}
function main() {
  var total = 0;
  for (var r = 0; r < 300; r++) total += walk(build(100));
  return total;
}
main();
