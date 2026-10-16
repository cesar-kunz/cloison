// Kernel: extracting a plan: starting from the input steps, add each step whose inputs are known, mark its output, queue the steps that consume it; then run the plan.
// cloison: `inputsKnown`, `output` and `execute` see three classes; the worklist is the caller's collection (`todo = sources`).
// features: loop, call, object, new, proto, call-method, array, null
// expect: 5501000
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
function Node(v, stay) { this.value = v; this.users = new Coll(); this.by = null; this.mark = 0; this.stay = stay; }
function Step(out) { this.out = out; out.by = this; }
Step.prototype.output = function () { return this.out; };
Step.prototype.isInput = function () { return false; };
function Input(out) { Input.parent.call(this, out); }
inherit(Input, Step);
Input.prototype.isInput = function () { return true; };
Input.prototype.inputsKnown = function (mark) { return true; };
Input.prototype.execute = function () {};
function Copy(a, out) { Copy.parent.call(this, out); this.a = a; a.users.add(this); }
inherit(Copy, Step);
Copy.prototype.inputsKnown = function (mark) {
  var i = this.a;
  return i.mark == mark || i.stay || i.by == null;
};
Copy.prototype.execute = function () { this.out.value = this.a.value; };
function Sum(a, b, out) {
  this.a = a;
  this.b = b;
  Sum.parent.call(this, out);
  a.users.add(this);
  b.users.add(this);
}
inherit(Sum, Step);
Sum.prototype.inputsKnown = function (mark) {
  return (this.a.mark == mark || this.a.stay) && (this.b.mark == mark || this.b.stay);
};
Sum.prototype.execute = function () { this.out.value = this.a.value + this.b.value; };
function Planner() { this.mark = 0; }
Planner.prototype.newMark = function () { return ++this.mark; };
Planner.prototype.consumers = function (v, todo) {
  var by = v.by, us = v.users;
  for (var i = 0; i < us.size(); i++) {
    var s = us.at(i);
    if (s != by) todo.add(s);
  }
};
Planner.prototype.makePlan = function (sources) {
  var mark = this.newMark();
  var plan = new Coll();
  var todo = sources;
  while (todo.size() > 0) {
    var s = todo.removeFirst();
    if (s.output().mark != mark && s.inputsKnown(mark)) {
      plan.add(s);
      s.output().mark = mark;
      this.consumers(s.output(), todo);
    }
  }
  return plan;
};
Planner.prototype.extract = function (steps) {
  var sources = new Coll();
  for (var i = 0; i < steps.size(); i++) {
    var s = steps.at(i);
    if (s.isInput()) sources.add(s);
  }
  return this.makePlan(sources);
};
function main() {
  var nodes = [new Node(0, false)], steps = new Coll();
  steps.add(new Input(nodes[0]));
  for (var i = 0; i < 100; i++) {
    var out = new Node(0, false);
    steps.add(i % 3 == 2 ? new Sum(nodes[i], new Node(i, true), out) : new Copy(nodes[i], out));
    nodes.push(out);
  }
  var p = new Planner(), total = 0;
  for (var r = 0; r < 2000; r++) {
    var plan = p.extract(steps);
    nodes[0].value = r;
    for (var i = 0; i < plan.size(); i++) plan.at(i).execute();
    total += plan.size() + nodes[100].value;
  }
  return total;
}
main();
