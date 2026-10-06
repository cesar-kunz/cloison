// Kernel: build a graph by constructor side effects, then remove edges by in-place compaction.
// features: loop, call, object, new, proto, array, null
// expect: 20600
function inherit(child, parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  child.prototype = new Tmp();
  child.parent = parent;
}
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
Coll.prototype.removeFirst = function () { return this.elms.pop(); };
Coll.prototype.remove = function (elm) {
  var index = 0, skipped = 0;
  for (var i = 0; i < this.elms.length; i++) {
    var value = this.elms[i];
    if (value != elm) { this.elms[index] = value; index++; }
    else skipped++;
  }
  for (var i = 0; i < skipped; i++) this.elms.pop();
};
function Node(i) { this.value = i; this.edges = new Coll(); this.by = null; }
Node.prototype.removeEdge = function (e) { this.edges.remove(e); if (this.by == e) this.by = null; };
function Edge(a, b) { this.a = a; this.b = b; a.edges.add(this); b.edges.add(this); b.by = this; }
Edge.prototype.destroy = function () { this.a.removeEdge(this); this.b.removeEdge(this); };
function main() {
  var total = 0;
  for (var r = 0; r < 200; r++) {
    var nodes = [], edges = [];
    for (var i = 0; i < 100; i++) nodes.push(new Node(i));
    for (var i = 1; i < 100; i++) {
      edges.push(new Edge(nodes[i - 1], nodes[i]));
      edges.push(new Edge(nodes[0], nodes[i]));
    }
    total += nodes[0].edges.size();
    for (var i = edges.length - 1; i >= 0; i -= 2) edges[i].destroy();
    total += nodes[0].edges.size() + nodes[50].edges.size() + (nodes[50].by != null ? 1 : 0);
  }
  return total;
}
main();
