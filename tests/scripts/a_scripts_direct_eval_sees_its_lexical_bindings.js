// A script's direct `eval` sees the script's lexical bindings (its
// `let`s, `const`s, classes, a block's), reads and writes them, and its
// `var`s are still the global object's.
// expect: 1 function 5 undefined 3 TypeError SyntaxError 6 5 6

let c = 1; const q = 2; class K {}
var r = [eval("c"), eval("typeof K")];
eval("c = 5"); r.push(c, typeof globalThis.c);
{ let b = 3; r.push(eval("b")); }
try { eval("q = 4"); } catch (e) { r.push(e.name); }
try { eval("var c = 2"); } catch (e) { r.push(e.name); }
eval("var v = 6"); r.push(globalThis.v);
for (let i = 0; i < 2; i++) r.push(eval("i + c"));
r.join(" ");
