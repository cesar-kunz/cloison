// Annex B's function in a block of an indirect `eval` or a script's
// direct one, by the name of a global `let` made after the code began
// (so its `var` was made), is stored to the `let` (B.3.2.3's
// SetMutableBinding, the global scope's block-scoped binding first,
// 9.1.1.4.5): a `const`'s a TypeError, one in its TDZ a ReferenceError.
// Such a binding made before the code began still stops it (#159). What
// the code's declarations decided holds whatever becomes of the global
// object: a `var` made, then deleted, the global object then made
// non-extensible, is still stored (an inherited setter called); one not
// made on a non-extensible global object is not, though a `let` of the
// name comes since. JSC's values (SpiderMonkey and V8 throw for those).
// expect: function undefined function undefined TypeError 1 ReferenceError 5 5 function setter function number number

var r = [];
(0, eval)("$262.evalScript('let q = 1'); { function q() {} }"); r.push(typeof q, typeof globalThis.q);
eval("$262.evalScript('let q2 = 1'); { function q2() {} }"); r.push(typeof q2, typeof globalThis.q2);
try { (0, eval)("$262.evalScript('const c = 1'); { function c() {} }"); } catch (e) { r.push(e.name, c); }
try { $262.evalScript("let t = (() => { throw 0; })()"); } catch (e) {}
try { (0, eval)("{ function t() {} }"); } catch (e) { r.push(e.name); }
try { (0, eval)("try { $262.evalScript('let u = (() => { throw 0; })()'); } catch (e) {} { function u() {} }"); } catch (e) { r.push(e.name); }
q = 5; (0, eval)("{ function q() {} }"); r.push(q);
(0, eval)("let z; { function q() {} }"); r.push(q);
(0, eval)("let z; $262.evalScript('let q5 = 1'); { function q5() {} }"); r.push(typeof q5);
Object.defineProperty(Object.prototype, "f", { set(v) { r.push("setter " + typeof v); }, configurable: true });
(0, eval)("delete globalThis.f; Object.preventExtensions(globalThis); { function f() {} }");
(0, eval)("$262.evalScript('let k = 1'); { function k() {} }"); r.push(typeof k);
eval("$262.evalScript('let k2 = 1'); { function k2() {} }"); r.push(typeof k2);
r.join(" ");
