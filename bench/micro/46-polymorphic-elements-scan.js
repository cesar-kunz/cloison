// Scanning a collection of objects of four shapes, calling a predicate on each, collecting the ones that pass.
// toi: element loads see four shapes; `isSatisfied` is two implementations, one reading a global constant.
// features: loop, call, object, new, proto, call-method, array, global, null
// expect: 10333322
function inherit(child, parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  child.prototype = new Tmp();
  child.parent = parent;
}
var Dir = new Object();
Dir.NONE = 0;
Dir.FORWARD = 1;
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
Coll.prototype.removeFirst = function () { return this.elms.pop(); };
function One(s) { this.s = s; this.sat = false; }
One.prototype.isSatisfied = function () { return this.sat; };
function Stay(s) { Stay.parent.call(this, s); }
inherit(Stay, One);
function Edit(s) { Edit.parent.call(this, s); this.edits = 0; }
inherit(Edit, One);
function Two(s) { this.s = s; this.dir = Dir.NONE; }
Two.prototype.isSatisfied = function () { return this.dir != Dir.NONE; };
function Eq(s) { Eq.parent.call(this, s); }
inherit(Eq, Two);
function Scale(s) { this.dir = Dir.NONE; this.k = 2; Scale.parent.call(this, s); }
inherit(Scale, Two);
function Var() { this.constraints = new Coll(); this.determinedBy = null; }
function consumers(v, out) {
  var determining = v.determinedBy;
  var cc = v.constraints;
  for (var i = 0; i < cc.size(); i++) {
    var c = cc.at(i);
    if (c != determining && c.isSatisfied()) out.add(c);
  }
}
function main() {
  var v = new Var(), all = [];
  for (var i = 0; i < 40; i++) {
    var k = i & 3;
    var c = k == 0 ? new Stay(i) : k == 1 ? new Edit(i) : k == 2 ? new Eq(i) : new Scale(i);
    v.constraints.add(c);
    all.push(c);
  }
  v.determinedBy = all[5];
  var total = 0;
  for (var r = 0; r < 20000; r++) {
    for (var i = 0; i < 40; i++) {
      var c = all[i], on = ((i + r) % 3) != 0;
      if (i & 2) c.dir = on ? Dir.FORWARD : Dir.NONE; else c.sat = on;
    }
    var out = new Coll();
    consumers(v, out);
    while (out.size() > 0) total += out.removeFirst().s;
  }
  return total;
}
main();
