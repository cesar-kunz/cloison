// A local worklist passed to a helper that only pushes into it.
// cloison: stays flat only if the helper is inlined (or gets a field-passing variant).
// features: loop, call, object, new, proto, array, null
// expect: 9950000
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.size = function () { return this.elms.length; };
Coll.prototype.removeFirst = function () { return this.elms.pop(); };
function N(k) { this.k = k; this.next = []; this.mark = 0; }
function pushSuccessors(n, coll) {
  for (var i = 0; i < n.next.length; i++) coll.add(n.next[i]);
}
function visit(root, mark) {
  var todo = new Coll(), count = 0;
  todo.add(root);
  while (todo.size() > 0) {
    var n = todo.removeFirst();
    if (n.mark == mark) continue;
    n.mark = mark;
    count += n.k;
    pushSuccessors(n, todo);
  }
  return count;
}
function main() {
  var nodes = [];
  for (var i = 0; i < 200; i++) nodes.push(new N(i));
  for (var i = 0; i < 200; i++) {
    nodes[i].next.push(nodes[(i * 7 + 1) % 200]);
    nodes[i].next.push(nodes[(i * 13 + 5) % 200]);
  }
  var sum = 0;
  for (var m = 1; m <= 500; m++) sum += visit(nodes[m % 200], m);
  return sum;
}
main();
