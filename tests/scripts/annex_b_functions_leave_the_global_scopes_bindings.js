// Annex B's function in a block of an indirect `eval` or another
// script, by the name of the global scope's block-scoped binding, makes
// no `var` and is no error (B.3.2.2, B.3.2.3: HasLexicalDeclaration);
// it stays its block's, the binding keeps its value, also a `const`'s
// and one in its TDZ (#95).
// expect: number undefined 2 false number false 2 undefined function undefined 3

var r = [];
let a = 1; const c = 2;
(0, eval)("{ function a() {} }"); r.push(typeof a, typeof globalThis.a);
(0, eval)("switch (1) { case 1: function c() {} }"); r.push(c, "c" in globalThis);
$262.evalScript("if (true) function a() {}"); r.push(typeof a, "a" in globalThis);
$262.evalScript("{ function c() {} } var sv;"); r.push(c, typeof sv);
(0, eval)("{ function ny() {} }"); r.push(typeof ny);
$262.evalScript("r.push((0, eval)('{ function late() {} } typeof globalThis.late')); let late = 3;");
r.push(late);
r.join(" ");
