// Kernel: worklist propagation of "max-of" ranks downstream through a graph, with marks.
// features: loop, call, object, new, proto, array, global, null
// expect: 131000
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
function Rank(v) { this.v = v; }
Rank.maxOf = function (a, b) { return a.v > b.v ? a : b; };
var LEVELS = [new Rank(0), new Rank(1), new Rank(2), new Rank(3), new Rank(4)];
function Node(i) { this.links = new Coll(); this.by = null; this.level = LEVELS[4]; this.mark = 0; }
function Link(a, b, s) { this.a = a; this.b = b; this.rank = s; a.links.add(this); b.links.add(this); b.by = this; }
Link.prototype.output = function () { return this.b; };
Link.prototype.recalc = function () { this.b.level = Rank.maxOf(this.rank, this.a.level); };
function Graph() { this.mark = 0; }
Graph.prototype.consumers = function (n, coll) {
  var cc = n.links;
  for (var i = 0; i < cc.size(); i++) {
    var l = cc.at(i);
    if (l != n.by) coll.add(l);
  }
};
Graph.prototype.propagate = function (start) {
  var mark = ++this.mark, todo = new Coll(), steps = 0;
  todo.add(start);
  while (todo.size() > 0) {
    var l = todo.removeFirst();
    if (l.output().mark == mark) continue;
    l.output().mark = mark;
    l.recalc();
    steps++;
    this.consumers(l.output(), todo);
  }
  return steps;
};
function main() {
  var g = new Graph(), nodes = [];
  for (var i = 0; i < 200; i++) nodes.push(new Node(i));
  var first = null;
  for (var i = 1; i < 200; i++) {
    var l = new Link(nodes[(i - 1) >> 1], nodes[i], LEVELS[i % 4]);
    if (i == 1) first = l;
  }
  var total = 0;
  for (var r = 0; r < 1000; r++) {
    nodes[0].level = LEVELS[r % 5];
    total += g.propagate(first) + nodes[199].level.v;
  }
  return total;
}
main();
