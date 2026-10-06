// A one-field wrapper class around an array, used only through one-line methods.
// features: loop, call, object, new, proto, array
// expect: 1225000
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
Coll.prototype.removeFirst = function () { return this.elms.pop(); };
function main() {
  var total = 0;
  for (var r = 0; r < 1000; r++) {
    var c = new Coll();
    for (var i = 0; i < 100; i++) c.add(i);
    for (var i = 0; i < c.size(); i++) total += c.at(i);
    while (c.size() > 50) total -= c.removeFirst();
  }
  return total;
}
main();
