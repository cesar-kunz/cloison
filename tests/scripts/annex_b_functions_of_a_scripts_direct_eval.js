// So in a script's direct `eval` in a block (#95): Annex B's function
// by a global block-scoped binding's name is no error and makes no
// `var`; by another name it makes the global `var` and sets it.
// expect: number 2 function number undefined 2 function true undefined 3

var r = [];
$262.evalScript("let x = 1; const kc = 2;");
{ let local; r.push(eval("{ function x() {} } typeof x"), eval("{ function kc() {} } kc"), eval("{ function q() {} } typeof q")); }
r.push(typeof x, typeof globalThis.x, kc, typeof q, Object.getOwnPropertyDescriptor(globalThis, "q").configurable);
$262.evalScript("{ let l; r.push(eval('{ function late() {} } typeof globalThis.late')); } let late = 3;");
r.push(late);
r.join(" ");
