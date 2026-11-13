// `super.x` in a direct `eval`'s code in a method (an object literal's,
// a class's, static, an accessor, a field's initializer), or in an arrow
// in one, or in an `eval` in that code: its home object's, as in the
// method (PerformEval's `inMethod`, 19.2.1.1). Not in a function other
// than a method, nor in a function in the `eval`'s code (#174).
// expect: method:7 arrow:7 arrow-in-eval:7 call:m2b nested:7 scope:8 static:S getter:9 setter:5 field:14 fn-in-eval:SyntaxError object:6 plain:SyntaxError top:SyntaxError indirect:SyntaxError loop:1600 before:ReferenceError this:ReferenceError after:7

var out = [];
function t(name, f) { try { out.push(name + ":" + f()); } catch (e) { out.push(name + ":" + e.name); } }
class A { get x() { return 7 } static get s() { return "S" } m2() { return "m2" + this.tag } }
class B extends A {
  constructor() { super(); this.tag = "b"; }
  m() { return eval('super.x') }
  n() { return (() => eval('super.x'))() }
  o() { return eval('(() => super.x)()') }
  p() { return eval('super.m2()') }
  q() { return eval('eval("super.x")') }
  r() { return eval('super["x"] + 1; // eval') }
  static st() { return eval('super.s') }
  get g() { return eval('super.x + 2') }
  set w(v) { eval('super.zz = v') }
  u() { return eval('(function () { return super.x })()') }
  f = eval('super.x * 2');
}
var b = new B();
t("method", () => b.m());
t("arrow", () => b.n());
t("arrow-in-eval", () => b.o());
t("call", () => b.p());
t("nested", () => b.q());
t("scope", () => b.r());
t("static", () => B.st());
t("getter", () => b.g);
t("setter", () => { b.w = 5; return b.zz; });
t("field", () => b.f);
t("fn-in-eval", () => b.u());
var o = { __proto__: { y: 3 }, m() { return eval('super.y') } };
t("object", () => o.m() + o.m.call({}));
t("plain", () => (function () { return eval('super.x') })());
t("top", () => eval('super.x'));
t("indirect", () => b.m.call({}) + new (class extends A { m() { return (0, eval)('super.x') } })().m());
var sum = 0;
function plain() { return eval('super.x') }
for (var i = 0; i < 200; i++) { sum += b.m(); try { plain() } catch (e) { sum += 1 } }
t("loop", () => sum);
class C extends A {
  constructor() {
    try { eval('super.x') } catch (e) { out.push("before:" + e.name) }
    try { eval('this') } catch (e) { out.push("this:" + e.name) }
    super();
    out.push("after:" + eval('super.x'));
  }
}
new C();
out.join(" ");
