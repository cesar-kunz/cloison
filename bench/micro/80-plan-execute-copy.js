// Kernel: executing a plan (a collection of steps) over a chain, many times per input change.
// features: loop, call, object, new, proto, call-method, array, global
// expect: 3999
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
var FORWARD = 1, BACKWARD = -1;
function Cell(v) { this.value = v || 0; }
function Step() {}
Step.prototype.isNoop = function () { return false; };
function Input(cell) { Input.parent.call(this); this.cell = cell; }
inherit(Input, Step);
Input.prototype.execute = function () {};
function Copy(a, b, dir) { Copy.parent.call(this); this.a = a; this.b = b; this.dir = dir; }
inherit(Copy, Step);
Copy.prototype.input = function () { return this.dir == FORWARD ? this.a : this.b; };
Copy.prototype.output = function () { return this.dir == FORWARD ? this.b : this.a; };
Copy.prototype.execute = function () { this.output().value = this.input().value; };
function Plan() { this.v = new Coll(); }
Plan.prototype.add = function (s) { this.v.add(s); };
Plan.prototype.size = function () { return this.v.size(); };
Plan.prototype.stepAt = function (i) { return this.v.at(i); };
Plan.prototype.execute = function () {
  for (var i = 0; i < this.size(); i++) this.stepAt(i).execute();
};
function main() {
  var cells = [];
  for (var i = 0; i <= 100; i++) cells.push(new Cell());
  var plan = new Plan();
  plan.add(new Input(cells[0]));
  for (var i = 0; i < 100; i++) plan.add(new Copy(cells[i], cells[i + 1], FORWARD));
  var ok = 0;
  for (var r = 0; r < 2000; r++) {
    cells[0].value = r;
    plan.execute();
    if (cells[100].value == r) ok++;
  }
  return ok + cells[50].value;
}
main();
