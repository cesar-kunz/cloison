// Call sites over four classes whose implementations live at different depths of the prototype chain.
// toi: `weight` is found one or two levels up, `isInput` one or three: each site's cases differ in depth.
// features: loop, call, object, new, proto, call-method, array
// expect: 65750000
function inherit(child, parent) {
  function Tmp() {}
  Tmp.prototype = parent.prototype;
  child.prototype = new Tmp();
  child.parent = parent;
}
function V(x) { this.x = x; }
function Base(s) { this.s = s; }
Base.prototype.isInput = function () { return false; };
function One(v, s) { One.parent.call(this, s); this.out = v; this.sat = false; }
inherit(One, Base);
One.prototype.weight = function () { return this.s + this.out.x; };
function Stay(v, s) { Stay.parent.call(this, v, s); }
inherit(Stay, One);
function Edit(v, s) { Edit.parent.call(this, v, s); }
inherit(Edit, One);
Edit.prototype.isInput = function () { return true; };
function Two(a, b, s) { Two.parent.call(this, s); this.a = a; this.b = b; this.dir = 1; }
inherit(Two, Base);
Two.prototype.output = function () { return this.dir == 1 ? this.b : this.a; };
Two.prototype.weight = function () { return this.s + this.output().x; };
function Eq(a, b, s) { Eq.parent.call(this, a, b, s); }
inherit(Eq, Two);
function Scale(a, b, k, s) { this.dir = 1; this.k = k; Scale.parent.call(this, a, b, s); }
inherit(Scale, Two);
Scale.prototype.weight = function () { return this.s + this.output().x * this.k; };
function main() {
  var objs = [];
  for (var i = 0; i < 100; i++) {
    var a = new V(i), b = new V(i + 1), k = i & 3;
    objs.push(k == 0 ? new Stay(a, 1) : k == 1 ? new Edit(a, 2) : k == 2 ? new Eq(a, b, 3) : new Scale(a, b, 2, 4));
  }
  var sum = 0;
  for (var r = 0; r < 10000; r++)
    for (var i = 0; i < 100; i++) {
      var o = objs[i];
      sum += o.weight();
      if (o.isInput()) sum++;
    }
  return sum;
}
main();
