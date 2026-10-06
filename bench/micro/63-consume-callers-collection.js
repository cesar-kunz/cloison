// A callee draining a collection its caller built (aliasing: `var todo = sources`).
// features: loop, call, object, new, proto, array
// expect: 116750
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.size = function () { return this.elms.length; };
Coll.prototype.removeFirst = function () { return this.elms.pop(); };
function drain(sources) {
  var todo = sources, out = new Coll();
  while (todo.size() > 0) {
    var x = todo.removeFirst();
    out.add(x * 2);
    if (x > 3) todo.add(x - 3);
  }
  return out;
}
function build(n) {
  var s = new Coll();
  for (var i = 0; i < n; i++) s.add(i);
  return drain(s);
}
function main() {
  var total = 0;
  for (var r = 0; r < 2000; r++) {
    var s = new Coll();
    total += build(10 + (r & 15)).size();
    total += s.size();
  }
  return total;
}
main();
