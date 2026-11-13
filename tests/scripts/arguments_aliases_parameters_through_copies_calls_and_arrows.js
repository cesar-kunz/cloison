// A sloppy function's `arguments` is mapped to its parameters wherever
// aliasing could show: `arguments` stored or passed on (not to `apply`),
// written in an arrow.
// expect: 2 9 4 8 1 100

var r = [];
function a1(a) { var x = arguments; x[0] = 2; return a; } r.push(a1(1));
function g(x) { x[0] = 9; } function a2(a) { g(arguments); return a; } r.push(a2(1));
function a3(a) { (() => arguments[0] = 4)(); return a; } r.push(a3(1));
function a6(a) { return Math.max.apply(null, arguments); } r.push(a6(7, 8));
function a8(a) { "use strict"; var x = arguments; x[0] = 2; return a; } r.push(a8(1));
function rec(a, n) { var args = arguments; if (n > 0) rec(a + 1, n - 1); args[0] = 100; return a; } r.push(rec(1, 3));
r.join(" ");
