// A sloppy function with a direct `eval` keeps its parameters in its
// scope object: its `arguments` is mapped to them there, both ways
// (cloison-fuzz BUGS.md #19: they weren't mapped at all).
// expect: 99 undefinedundefined 55 7  9true 6 1undefined -43050

function f(a, b) { arguments[1] = 9; return b + "" + eval("b"); }
function g(a, b) { b = 5; return arguments[1] + "" + eval("arguments[1]"); }
function h(a, b) { eval("b = 7"); return arguments[1]; }
function k(a, b) { arguments[0] = 9; if (0) eval(""); return a + "" + (b === undefined); }
function m(a) { eval("arguments[0] = 6"); return a; }
function n(a, b) { eval(""); b = 3; return arguments.length + "" + arguments[1]; }
var s = 0;
for (var i = 0; i < 300; i++) s += m(i) - i;
[f(1, 2), f(1), g(1, 2), h(1, 2), h(1), k(1), m(1), n(1), s].join(' ');
