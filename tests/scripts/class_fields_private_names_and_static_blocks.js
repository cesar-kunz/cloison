// Class fields (computed, static), private fields, methods and
// accessors (static too, with brand checks and `#x in o`), and static
// blocks, in the order the spec defines.
// expect: fields 1,2,3,c,1,fn,true, | set 7 | method m7 | accessor g5 | statics 10,20,sm,11,2,true | has true,false | hasPrim !TypeError | keys ["x","y","comp1","arrow","fn","noinit","log"]07 | brand !TypeError | brandSet !TypeError | distinct 7,100 | derived q104,1,3,true | defaultDerived 2,1 | arrowSuper 1 | otherClass !TypeError | shadow !TypeError | update 1,2,12 | logical 5 | optional null | writeMethod !TypeError | noSetter !TypeError | order s,a,b,ctor | ctorReturn {"y":2} | computedOnce {"0":2,"1":2}{"0":2,"1":2}2 | frozen 2 | spreadNoPriv {} | staticThis 1,2,3 | oddNames 1,2,3,4,true,5 | asi 6 | loopClasses 2 | perEval !TypeError | toString true

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
function t(name, f) { try { log(name, f()); } catch (e) { log(name, "!" + e.name); } }
class A {
  x = 1;
  y = this.x + 1;
  #p = 3;
  static s = 10;
  static #sp = 20;
  ["comp" + 1] = "c";
  arrow = () => this.x;
  fn = function () {};
  noinit;
  get p() { return this.#p; }
  set p(v) { this.#p = v; }
  #m() { return "m" + this.#p; }
  callM() { return this.#m(); }
  get #g() { return "g"; }
  set #g(v) { this.log = v; }
  useG() { this.#g = 5; return this.#g + this.log; }
  static sp() { return A.#sp; }
  static #sm() { return "sm"; }
  static callSm() { return this.#sm(); }
  static has(o) { return #p in o; }
  static { A.block = A.s + 1; var local = 2; A.local = local; }
  static self = this;
}
var a = new A();
t("fields", () => [a.x, a.y, a.p, a.comp1, a.arrow(), a.fn.name, "noinit" in a, a.noinit].join());
t("set", () => { a.p = 7; return a.p; });
t("method", () => a.callM());
t("accessor", () => a.useG());
t("statics", () => [A.s, A.sp(), A.callSm(), A.block, A.local, A.self === A].join());
t("has", () => [A.has(a), A.has({})].join());
t("hasPrim", () => A.has(1));
t("keys", () => JSON.stringify(Object.keys(a)) + Object.getOwnPropertySymbols(a).length + Reflect.ownKeys(a).length);
t("brand", () => A.prototype.callM.call({}));
t("brandSet", () => Object.getOwnPropertyDescriptor(A.prototype, "p").set.call({}, 1));
t("distinct", () => { var b = new A(); b.p = 100; return a.p + "," + b.p; });
class B extends A {
  z = this.x * 10;
  #q = "q";
  constructor(k) { super(); this.k = k; }
  q() { return this.#q + this.z + this.k; }
}
var b = new B(4);
t("derived", () => [b.q(), b.x, b.p, b instanceof A].join());
class C extends A { w = 2; }
t("defaultDerived", () => [new C().w, new C().x].join());
class D extends A { v = 1; constructor() { var f = () => super(); f(); this.after = this.v; } }
t("arrowSuper", () => new D().after);
class E2 { #x = 1; static get(o) { return o.#x; } }
t("otherClass", () => E2.get(new A()));
class F { #x = 1; m() { class G { #x = 2; read(o) { return o.#x; } } return new G().read(this); } }
t("shadow", () => new F().m());
class H { static #c = 0; static inc() { return ++H.#c; } static add() { H.#c += 10; return H.#c; } }
t("update", () => [H.inc(), H.inc(), H.add()].join());
class I { #v = null; f() { this.#v ??= 5; this.#v ||= 6; return this.#v; } g() { return this?.#v; } }
t("logical", () => new I().f());
t("optional", () => new I().g());
class J { #m() {} tryWrite() { this.#m = 1; } }
t("writeMethod", () => new J().tryWrite());
class K { get #r() { return 1; } w() { this.#r = 2; } }
t("noSetter", () => new K().w());
var order = [];
class L { a = order.push("a"); static s = order.push("s"); b = order.push("b"); constructor() { order.push("ctor"); } }
new L();
t("order", () => order.join());
class M { x = 1; constructor() { return { y: 2 }; } }
t("ctorReturn", () => JSON.stringify(new M()));
var counter = 0;
class N { [counter++] = counter; [counter++] = counter; }
t("computedOnce", () => JSON.stringify(new N()) + JSON.stringify(new N()) + counter);
class O { #x = 1; static peek(o) { return o.#x; } constructor() { Object.freeze(this); } bump() { this.#x++; return this.#x; } }
t("frozen", () => new O().bump());
t("spreadNoPriv", () => JSON.stringify({ ...new O() }));
class P { static x = 1; static y = P.x + 1; static z = this.y + 1; }
t("staticThis", () => [P.x, P.y, P.z].join());
class Q { 'quoted' = 1; 42 = 2; static async = 3; get = 4; set; static = 5; }
var q = new Q();
t("oddNames", () => [q.quoted, q[42], Q.async, q.get, "set" in q, q.static].join());
class R { x = 1
  y = 2
  #z = 3
  sum() { return this.x + this.y + this.#z } }
t("asi", () => new R().sum());
for (var i = 0; i < 3; i++) { class S { #v = i; static read(o) { return o.#v; } } if (i == 2) t("loopClasses", () => S.read(new S())); }
var Ss = []; for (let i = 0; i < 2; i++) Ss.push(class { #v = i; static read(o) { return o.#v; } });
t("perEval", () => Ss[0].read(new Ss[1]()));
t("toString", () => String(class X { y = 1 }).length > 0);
out.join(" | ");
