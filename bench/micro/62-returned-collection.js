// A function building a collection and returning it to its caller, which consumes it.
// toi: internal heap object (returned from a non-inlined call); freed with its caller's region.
// features: loop, call, object, new, proto, array
// expect: 4308524
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
function collect(n) {
  var out = new Coll();
  for (var i = 0; i < n; i++) if (i % 3 != 0) out.add(i);
  return out;
}
function main() {
  var sum = 0;
  for (var r = 0; r < 3000; r++) {
    var c = collect(50 + (r & 31));
    for (var i = 0; i < c.size(); i++) sum += c.at(i);
  }
  return sum;
}
main();
