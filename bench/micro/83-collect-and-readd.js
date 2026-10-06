// Kernel: collect unsatisfied items into a returned collection; re-add them lowest rank first.
// features: loop, call, object, new, proto, array, null, global
// expect: 5637500
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
var LEVELS = [new Rank(0), new Rank(1), new Rank(2), new Rank(3)];
Rank.prototype.next = function () { return LEVELS[this.v + 1]; };
function Item(k, s) { this.k = k; this.rank = s; this.on = (k % 3) != 0; this.next = null; }
function collect(head) {
  var out = new Coll(), todo = new Coll();
  todo.add(head);
  while (todo.size() > 0) {
    var it = todo.removeFirst();
    if (!it.on) out.add(it);
    if (it.next != null) todo.add(it.next);
  }
  return out;
}
function readd(items) {
  var s = LEVELS[0], added = 0;
  do {
    for (var i = 0; i < items.size(); i++) {
      var it = items.at(i);
      if (it.rank == s) { added += it.k; it.on = true; }
    }
    s = s.next();
  } while (s != LEVELS[3]);
  return added;
}
function main() {
  var head = null;
  for (var i = 0; i < 300; i++) { var it = new Item(i, LEVELS[i & 3]); it.next = head; head = it; }
  var total = 0;
  for (var r = 0; r < 500; r++) {
    for (var it = head; it != null; it = it.next) it.on = ((it.k + r) % 3) != 0;
    var u = collect(head);
    total += u.size() + readd(u);
  }
  return total;
}
main();
