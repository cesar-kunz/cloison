// `delete super.x` is a ReferenceError (13.5.1.2, a super reference),
// in strict code or not, in an arrow, in a direct `eval`'s code; after
// the key's expression is evaluated, but not made a property key, and
// after `this`, so before `super()` the key is not evaluated
// (13.3.7.1). The property stays.
// expect: a:ReferenceError key b:ReferenceError c:ReferenceError d:ReferenceError e:ReferenceError f:1 before:ReferenceError

var log = [];
function t(name, f) { try { log.push(name + ":" + f()); } catch (e) { log.push(name + ":" + e.name); } }
var key = { toString() { log.push("toString"); return "x"; } };
var o = {
  __proto__: { x: 1 },
  a() { return delete super.x; },
  b() { return delete super[(log.push("key"), key)]; },
  c() { "use strict"; return delete super.x; },
  d() { return eval('delete super.x'); },
  e() { return (() => delete super.x)(); },
  f() { try { delete super.x; } catch (e) {} return Object.getPrototypeOf(this).x; },
};
for (var m of "abcdef") t(m, () => o[m]());
class A {}
class B extends A { constructor() { try { delete super[(log.push("k2"), "x")]; } catch (e) { log.push("before:" + e.name); } super(); } }
new B();
log.join(" ");
