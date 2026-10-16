// A parent constructor calling a method that subclasses override, on an object still being built.
// cloison: the subclasses store fields before or after running the parent constructor, so in different orders.
// features: loop, call, object, new, proto, call-method, array
// expect: 269400
function inherit(child, parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  child.prototype = new Tmp();
  child.parent = parent;
}
function Port() { this.links = []; }
Port.prototype.attach = function (l) { this.links.push(l); };
function Link(a, b, w) {
  this.w = w;
  this.a = a;
  this.b = b;
  this.on = false;
  this.register();
}
Link.prototype.register = function () {
  this.a.attach(this);
  this.b.attach(this);
  this.on = true;
};
function Plain(a, b) { Plain.parent.call(this, a, b, 1); }
inherit(Plain, Link);
function Scaled(a, b, k, c, w) {
  this.on = false;
  this.k = k;
  this.c = c;
  Scaled.parent.call(this, a, b, w);
}
inherit(Scaled, Link);
Scaled.prototype.register = function () {
  Scaled.parent.prototype.register.call(this);
  this.k.attach(this);
  this.c.attach(this);
};
function main() {
  var k = new Port(), c = new Port(), total = 0;
  for (var r = 0; r < 600; r++) {
    var prev = new Port();
    for (var i = 0; i < 100; i++) {
      var p = new Port();
      var l = (i & 1) ? new Scaled(prev, p, k, c, 2) : new Plain(prev, p);
      if (l.on) total += l.w + prev.links.length;
      prev = p;
    }
    total += k.links.length + c.links.length;
    k.links = [];
    c.links = [];
  }
  return total;
}
main();
