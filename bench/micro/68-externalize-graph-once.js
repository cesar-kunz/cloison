// A graph built internally, then made reachable from a global once, then used.
// toi: one transitive externalization walk; it must not cost more than that.
// features: loop, call, object, new, proto, array, global, null, recursion
// expect: 167690240
var registry = new Object();
registry.root = null;
function Node(i) { this.value = i; this.kids = []; }
function build(depth, i) {
  var n = new Node(i);
  if (depth > 0) { n.kids.push(build(depth - 1, 2 * i)); n.kids.push(build(depth - 1, 2 * i + 1)); }
  return n;
}
function sum(n) {
  var s = n.value;
  for (var i = 0; i < n.kids.length; i++) s += sum(n.kids[i]);
  return s;
}
function main() {
  var total = 0;
  for (var r = 0; r < 40; r++) {
    var g = build(10, 1);
    total += sum(g);
    registry.root = g;          // externalizes all 2047 nodes
    total += sum(registry.root);
  }
  return total;
}
main();
