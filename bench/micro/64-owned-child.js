// An object that uniquely owns a collection, accessed through the owner and through a local alias.
// cloison: candidate for object inlining (B) when every use of the alias inlines.
// features: loop, call, object, new, proto, array
// expect: 35000000
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
function Owner() { this.items = new Coll(); this.mark = 0; }
Owner.prototype.add = function (x) { this.items.add(x); };
function sumVia(o) {
  var cc = o.items, s = 0;
  for (var i = 0; i < cc.size(); i++) s += cc.at(i);
  return s;
}
function main() {
  var owners = [];
  for (var i = 0; i < 50; i++) {
    var o = new Owner();
    for (var j = 0; j < 20; j++) o.add(i + j);
    owners.push(o);
  }
  var total = 0;
  for (var r = 0; r < 1000; r++) {
    for (var i = 0; i < owners.length; i++) {
      total += sumVia(owners[i]) + owners[i].items.size();
    }
  }
  return total;
}
main();
