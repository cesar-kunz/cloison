// A script's top-level `let`s, `const`s and classes are the global
// scope's (#95): indirect `eval`, `Function` and other scripts see them,
// before the global object's properties, and their stores reach them;
// their TDZ and `const`s hold there too, and none is deletable. A later
// script's binding shadows a global property compiled code read already.
// A `let` may not be another script's, but may shadow an `eval`'s `var`
// or a configurable global (GlobalDeclarationInstantiation, ECMA-262
// §16.1.7); an `eval`'s `var` may not be a global `let` (§19.2.1.3).
// expect: number numberfunction 2 1 5 undefined false TypeError SyntaxError SyntaxError SyntaxError false false number ReferenceError 7 number ReferenceError ReferenceError ReferenceError 200 1 3 SyntaxError

let y = 1; const k = 2; class C {}
globalThis.x = 1; let x = 2;
function early() { return z; }
var r = [(0, eval)("typeof y"), Function("return typeof y + typeof C")(), (0, eval)("x"), globalThis.x];
(0, eval)("y = 5"); r.push(y, typeof globalThis.y, "C" in globalThis);
try { (0, eval)("k = 3"); } catch (e) { r.push(e.name); }
try { (0, eval)("var y"); } catch (e) { r.push(e.name); }
{ try { eval("var y"); } catch (e) { r.push(e.name); } }
try { $262.evalScript("let y"); } catch (e) { r.push(e.name); }
r.push((0, eval)("delete y"), delete y, typeof y);
try { early(); } catch (e) { r.push(e.name); }
$262.evalScript("let z = 7");
r.push(early(), $262.evalScript("typeof z"));
try { $262.evalScript("w; let w = 1"); } catch (e) { r.push(e.name); }
try { w = 2; } catch (e) { r.push(e.name); }
try { (0, eval)("typeof w"); } catch (e) { r.push(e.name); }
globalThis.g = 1;
function rd() { var s = 0; for (var i = 0; i < 100; i++) s += g; return s; }
for (var i = 0; i < 50; i++) rd();
$262.evalScript("let g = 2");
r.push(rd(), globalThis.g);
eval("var ev");
$262.evalScript("let ev = 3; var sv;");
r.push(ev);
try { $262.evalScript("let sv"); } catch (e) { r.push(e.name); }
r.join(" ");
