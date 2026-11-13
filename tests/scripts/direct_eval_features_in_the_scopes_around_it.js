// A direct `eval`'s `super.x`, `delete super.x`, Annex B's block
// functions and an arrow's `eval` before `super()` in the scopes a
// direct `eval` sees: inside `with`s (their objects' properties found
// past the method's bindings, never its home object, which a Proxy's
// `has` doesn't see), in nested `eval`s and the functions of an
// `eval`'s code (a method's there its own home object), past 80
// bindings, and run 3000 times from kept code. SpiderMonkey's and
// JSC's values.
// expect: 10 1 2 !SyntaxError 8 1 2 ReferenceError1 number function number number 6 !ReferenceError 4501500 4498500 4498500 101 50

var log = [];
function t(f) { try { log.push(String(f())); } catch (e) { log.push("!" + e.name); } }
var P = { x: 1, y: 2 };
t(() => ({ __proto__: P, m() { with ({ x: 9 }) { return eval('super.x + x'); } } }).m());
t(() => ({ __proto__: P, m() { with ({ x: 9 }) { return eval('(() => eval("super.x"))()'); } } }).m());
t(() => ({ __proto__: P, m() { return eval('(() => { with ({}) return eval("super.y"); })()'); } }).m());
t(() => ({ __proto__: P, m() { return eval('(function () { return eval("super.x"); })()'); } }).m());
t(() => ({ __proto__: P, m() { let q = 3; return eval('let r = 4; (() => eval("super.x + q + r"))()'); } }).m());
t(() => ({ __proto__: P, m() { var p = new Proxy({}, { has(t, k) { if (typeof k == "string" && k[0] == "%") throw 1; return false; } }); with (p) { return eval('super.x'); } } }).m());
t(() => ({ __proto__: P, m() { return eval('({ __proto__: { x: 2 }, n() { return eval("super.x"); } }).n()'); } }).m());
t(() => ({ __proto__: P, m() { with ({}) { try { eval('delete super.x'); } catch (e) { return e.name + P.x; } } } }).m());
t(() => (function () { let f = 1; with ({}) { eval('{ function f() {} }'); } return typeof f; })());
t(() => (function () { let f = 1; return eval('(function () { eval("{ function f() {} }"); return typeof f; })()'); })());
t(() => (function () { try { throw 1; } catch (f) { with ({}) { eval('{ function f() {} }'); return typeof f; } } })());
t(() => (function () { let f = 1; eval('eval("{ function f() {} }")'); return typeof f; })());
class A { constructor() { this.a = 1; } }
with ({ r: 5 }) t(() => { class B extends A { constructor() { var q = (() => eval('r + 1'))(); super(); this.q = q; } } return new B().q; });
with ({}) t(() => { class B extends A { constructor() { (() => eval('this'))(); super(); } } return new B(); });
t(() => { var o = { __proto__: P, m(i) { with ({}) { let z = i; return eval('super.x + z'); } } }; var s = 0; for (let i = 0; i < 3000; i++) s += o.m(i); return s; });
t(() => { var s = 0; for (let i = 0; i < 3000; i++) s += (function () { let f = i; eval('{ function f() {} }'); return f; })(); return s; });
t(() => { var s = 0; for (let i = 0; i < 3000; i++) s += (function () { let q = i; with ({}) return eval('(() => eval("q"))()'); })(); return s; });
var L = ""; for (let i = 0; i < 100; i++) L += "let z" + i + " = " + i + ";";
t(() => eval('({ __proto__: P, m() { ' + L + ' with ({}) return eval("super.y + z99"); } })').m());
t(() => eval('(function () { ' + L + ' eval("{ function z50() {} }"); return z50; })')());
log.join(" ");
