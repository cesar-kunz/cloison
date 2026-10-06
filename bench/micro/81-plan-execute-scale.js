// Kernel: a plan of linear-map steps run forward, then backward, with shared scale/offset cells.
// features: loop, call, object, new, proto, call-method, array, global, double
// expect: 1474174.2714285739
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
function Input(cell) { Input.parent.call(this); this.cell = cell; }
inherit(Input, Step);
Input.prototype.execute = function () {};
function Scale(src, scale, offset, dst) {
  this.dir = FORWARD; this.scale = scale; this.offset = offset;
  Scale.parent.call(this);
  this.src = src; this.dst = dst;
}
inherit(Scale, Step);
Scale.prototype.execute = function () {
  if (this.dir == FORWARD) this.dst.value = this.src.value * this.scale.value + this.offset.value;
  else this.src.value = (this.dst.value - this.offset.value) / this.scale.value;
};
function Plan() { this.v = new Coll(); }
Plan.prototype.add = function (s) { this.v.add(s); };
Plan.prototype.execute = function () {
  for (var i = 0; i < this.v.size(); i++) this.v.at(i).execute();
};
function main() {
  var scale = new Cell(10), offset = new Cell(1000), steps = [];
  var srcs = [], dsts = [];
  for (var i = 0; i < 100; i++) {
    srcs.push(new Cell(i)); dsts.push(new Cell(i));
    steps.push(new Scale(srcs[i], scale, offset, dsts[i]));
  }
  var plan = new Plan();
  plan.add(new Input(scale));
  for (var i = 0; i < 100; i++) plan.add(steps[i]);
  var acc = 0;
  for (var r = 0; r < 500; r++) {
    scale.value = 1 + (r & 7);
    for (var i = 0; i < 100; i++) steps[i].dir = FORWARD;
    plan.execute();
    acc += dsts[99].value;
    for (var i = 0; i < 100; i++) { steps[i].dir = BACKWARD; dsts[i].value += 3; }
    plan.execute();
    acc += srcs[99].value;
  }
  return acc;
}
main();
