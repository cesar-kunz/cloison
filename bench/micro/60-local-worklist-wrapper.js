// A worklist wrapper allocated, filled and drained inside one function; it never leaves.
// toi: candidate for scalar replacement of the wrapper (A); the array stays a heap object.
// features: loop, call, object, new, proto, array
// expect: 854376
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.size = function () { return this.elms.length; };
Coll.prototype.removeFirst = function () { return this.elms.pop(); };
function walk(start) {
  var todo = new Coll(), seen = 0;
  todo.add(start);
  while (todo.size() > 0) {
    var n = todo.removeFirst();
    seen += n;
    if (n > 1) { todo.add(n >> 1); todo.add(n >> 2); }
  }
  return seen;
}
function main() {
  var sum = 0;
  for (var i = 0; i < 3000; i++) sum = (sum + walk(64 + (i & 63))) | 0;
  return sum;
}
main();
