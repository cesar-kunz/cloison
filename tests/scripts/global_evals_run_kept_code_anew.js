// Indirect `eval`s' code, and the global code's direct ones', is kept
// too: each run makes its `var`s and functions globals, its `let`s its
// own, its template objects (not kept), its errors, and runs in the
// realm of the `eval` called, as V8, SpiderMonkey, JavaScriptCore and
// QuickJS give.
// expect: 2 1 2 3 undefined number false undefined true 999000 0 1 SyntaxError SyntaxError false true true

var out = [];
var geval = eval;
for (var k = 0; k < 3; k++) geval("var gv" + (k & 1) + " = " + k + "; function gf() { return " + k + "; }");
out.push(gv0, gv1, gf());
for (var k = 0; k < 3; k++) { geval("var same = (typeof same == 'number' ? same : 0) + 1"); }
out.push(same);
geval("var dv = 1"); delete globalThis.dv; out.push(typeof dv); geval("var dv = 1"); out.push(typeof dv);
function tag(t) { return t; }
out.push(geval("tag`x`") === geval("tag`x`"));
geval("'use strict'; var sv = 1"); out.push(typeof sv);
out.push(geval("this") === globalThis);
var s = 0; for (var i = 0; i < 1000; i++) s += eval("i * 2"); out.push(s);
for (var k = 0; k < 2; k++) out.push(geval("let lx = k; lx"));
for (var k = 0; k < 2; k++) { try { geval("let q = 1; let q = 2;"); } catch (e) { out.push(e.constructor.name); } }
var r = $262.createRealm(), e2 = r.global.eval;
out.push(e2("Array") === Array, geval("Array") === Array, e2("Array") === r.global.Array);
out.join(" ")
