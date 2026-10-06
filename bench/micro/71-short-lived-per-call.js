// Per-call temporaries: a request object, a one-element collection and a result built from them.
// features: loop, call, object, new, proto, array
// expect: 450255000
function Coll() { this.elms = new Array(); }
Coll.prototype.add = function (e) { this.elms.push(e); };
Coll.prototype.at = function (i) { return this.elms[i]; };
Coll.prototype.size = function () { return this.elms.length; };
function Edit(target) { this.target = target; this.active = true; }
function Plan() { this.v = new Coll(); }
Plan.prototype.add = function (x) { this.v.add(x); };
Plan.prototype.run = function (value) {
  for (var i = 0; i < this.v.size(); i++) this.v.at(i).target.value = value;
};
function V() { this.value = 0; }
function extract(edits) {
  var plan = new Plan();
  for (var i = 0; i < edits.size(); i++) if (edits.at(i).active) plan.add(edits.at(i));
  return plan;
}
function change(v, x) {
  var edit = new Edit(v);
  var edits = new Coll();
  edits.add(edit);
  var plan = extract(edits);
  for (var i = 0; i < 10; i++) plan.run(x + i);
  edit.active = false;
}
function main() {
  var v = new V(), total = 0;
  for (var r = 0; r < 30000; r++) { change(v, r); total += v.value; }
  return total;
}
main();
