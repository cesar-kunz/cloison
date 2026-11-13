// Private fields and fields of derived classes in code hot enough to
// compile.
// expect: 4999 49990001 15001 12527500 12512500

class Vec { #x; #y; static count = 0;
  constructor(x, y) { this.#x = x; this.#y = y; Vec.count++; }
  get x() { return this.#x; }
  add(o) { return new Vec(this.#x + o.#x, this.#y + o.#y); }
  len2() { return this.#x * this.#x + this.#y * this.#y; } }
class Vec3 extends Vec { z = 0; #w = 1; constructor(x, y, z) { super(x, y); this.z = z; } w() { return this.#w++; } }
var acc = new Vec(0, 0), s = 0;
for (var i = 0; i < 5000; i++) { acc = acc.add(new Vec(i % 3, 1)); var v3 = new Vec3(1, 2, i); s += v3.z + v3.w() + v3.len2(); }
var res = [acc.x, acc.len2(), Vec.count, s];
class Pt { x = 1; y = 2; }
var t = 0; for (var j = 0; j < 5000; j++) { var p = new Pt(); p.x += j; t += p.x + p.y; } res.push(t); res.join(" ");
