// A script's direct `eval` in a block finds the global scope's
// block-scoped bindings before the global object's properties, as the
// script does (#95): it doesn't look in the global object as in a
// `with`'s, which shadowed a `let` by a global property of its name and
// let a store bypass a `const`.
// expect: 2 3 1 2 TypeError 2 1 5 6 number 6 6

var r = [];
globalThis.x = 1; let x = 2;
{ let local; r.push(eval("x")); eval("x = 3"); r.push(x, globalThis.x); }
globalThis.k = 1; const k = 2;
{ let local; r.push(eval("k")); try { eval("k = 9"); } catch (e) { r.push(e.name); } r.push(k, globalThis.k); }
try { throw 0; } catch (e) { r.push(eval("x + k + e")); }
{ let b = 5; eval("var dv = b + 1"); r.push(dv, typeof globalThis.dv); }
{ let b2 = 6; r.push(eval("eval('var nest = b2'); nest"), nest); }
r.join(" ");
