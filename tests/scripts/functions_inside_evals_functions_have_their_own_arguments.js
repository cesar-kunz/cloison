// A function inside one with a direct `eval` finds its own variables
// before the `eval`'s scope object (#135): the enclosing function's
// `arguments`, parameters and `var`s of the same names were read (and
// written) there instead, the `eval`'s code can't declare its.
// expect: 2 1 2 1 1 3 5 2 2 42 5 2 9 1 1 3 1 2 1 1 3 1 2 2 1 3 3 45450

var out = [];
function t(v) { out.push(String(v)); }
function outer() { function f() { return arguments.length + " " + arguments[0]; } eval(''); return f(1, 2); }
t(outer(7, 8, 9));
function outer2() { var g = function () { return arguments.length; }; eval(''); return g(1, 2); }
t(outer2(7, 8, 9));
function outer3() { function f() { return (function () { return arguments.length; })(1); } eval(''); return f(1, 2); }
t(outer3(7, 8, 9));
function s1() { function f() { "use strict"; return arguments.length; } eval(''); return f(1); }
t(s1(7, 8));
function a1() { var g = () => arguments.length; eval(''); return g(1); }
t(a1(7, 8, 9));
function a2() { function f() { return (() => arguments[0])(); } eval(''); return f(5); }
t(a2(7, 8, 9));
function e1() { function f() { return eval("arguments.length"); } eval(''); return f(1, 2); }
t(e1(7, 8, 9));
function e3() { function f() { eval(''); return arguments.length; } eval(''); return f(1, 2); }
t(e3(7, 8, 9));
function p1() { function f(arguments) { return arguments; } eval(''); return f(42); }
t(p1(7, 8, 9));
function p2() { function f() { var arguments = 5; return arguments; } eval(''); return f(1); }
t(p2(7));
function p3() { function f() { var arguments; return arguments.length; } eval(''); return f(1, 2); }
t(p3(7));
function m1() { function f(a) { arguments[0] = 9; return a; } eval(''); return f(1); }
t(m1(7));
function m2() { function f(a) { "use strict"; arguments[0] = 9; return a; } eval(''); return f(1); }
t(m2(7));
function m3() { function f(a = 0) { arguments[0] = 9; return a; } eval(''); return f(1); }
t(m3(7));
function m4() { function f(a) { a = 3; return arguments[0]; } eval(''); return f(1); }
t(m4(7));
function v1() { var x = 1; function f() { var x = 2; return x; } eval(''); f(); return x; }
t(v1());
function v2(x) { function f(x) { return x; } eval(''); return f(2) + " " + x; }
t(v2(1));
function v3() { var x = 1; function f() { return x; } eval('var y = 3'); return f() + " " + (function () { return y; })(); }
t(v3());
function v5() { function f() { var z = 1; return z; } eval('var z = 2'); return f() + " " + z; }
t(v5());
t(Function("function f() { return arguments.length; } eval(''); return f(1, 2);")(7, 8, 9));
function o1() { function f() { return arguments.length; } eval(''); return f(1) + " " + arguments.length; }
t(o1(7, 8, 9));
function w1() { function f() { var x = 2; with ({}) { return x + arguments.length; } } eval(''); return f(1); }
t(w1(7, 8));
function hot() { function f(a) { return arguments.length + a; } eval(''); var s = 0; for (var i = 0; i < 300; i++) s += f(i, 0); return s; }
t(hot(7, 8, 9));
out.join(' ');
