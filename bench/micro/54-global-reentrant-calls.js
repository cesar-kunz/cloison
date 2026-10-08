// Methods passing `this` to a method of a global object, which calls back into them, and they call the global again.
// toi: the global is reassigned per phase, each time to a new object of the same shape.
// features: loop, call, object, new, proto, global, null
// expect: 37050000
var hub = null;
function Hub() { this.mark = 0; this.done = 0; }
Hub.prototype.newMark = function () { return ++this.mark; };
Hub.prototype.add = function (c) {
  var mark = this.newMark();
  var over = c.satisfy(mark);
  while (over != null) over = over.satisfy(mark);
};
Hub.prototype.propagate = function (c, mark) {
  this.done += c.k;
  return c.k >= 0;
};
function Item(k, prev) {
  this.k = k;
  this.prev = prev;
  this.mark = 0;
  this.activate();
}
Item.prototype.activate = function () { hub.add(this); };
Item.prototype.satisfy = function (mark) {
  this.mark = mark;
  if (!hub.propagate(this, mark)) return null;
  var p = this.prev;
  return p != null && p.mark != mark && (this.k & 3) != 0 ? p : null;
};
function phase(n) {
  hub = new Hub();
  var prev = null;
  for (var i = 0; i < n; i++) prev = new Item(i, prev);
  return hub.done + hub.mark;
}
function main() {
  var total = 0;
  for (var r = 0; r < 3000; r++) total += phase(100);
  return total;
}
main();
