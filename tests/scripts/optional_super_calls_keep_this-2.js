// `super.m?.()` calls `m` with `this` as its receiver, like `super.m()`
// (#150); if `m` is null or undefined, the chain ends before the
// arguments are evaluated.
// expect: true:1 true:0 true:0 true:2 true:3 6 true:0 true:0    0 true true true  true

class A { m(...a) { return (this === o) + ":" + a.length } static s() { return this === B } }
var o, calls = 0; function f() { calls++ }
class B extends A {
  constructor() { super(); o = this; this.c = super.m?.(9) }
  t() {
    var k = "m";
    return [super.m?.(), super[k]?.(), super.m?.(1, 2), super.m?.(...[1, 2, 3]), super.m?.().length,
      (() => super.m?.())(), (super.m)(), super.nope?.(f()), super["no" + "pe"]?.(f()),
      super.nope?.().x, calls];
  }
  static ts() { return super.s?.() }
}
var b = new B(), r = [b.c, ...b.t(), B.ts()];
var p = { m() { return this === q } }, q = { __proto__: p, t() { return super.m?.() } };
var q2 = { t() { return super.m?.() } }; Object.setPrototypeOf(q2, { m() { return this === q2 } });
class N {} N.prototype.n = null;
class M extends N { t() { return super.n?.() } }
r.push(q.t(), q2.t(), new M().t());
var ok = true;
for (var i = 0; i < 300; i++) ok = ok && b.t()[0] === "true:0" && q.t() && B.ts();
r.push(ok);
r.join(" ");
