// `super.m?.()` calls `m` with `this` as its receiver, like `super.m()`
// (#150); if `m` is null or undefined, the chain ends before the
// arguments are evaluated.
// expect: true

class A { m() { return this === o; } } var o;
class B extends A { t() { return super.m?.(); } }
o = new B(); o.t();
