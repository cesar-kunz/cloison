// A function expression's own name, in scope in its body only.
// for: pdfjs (592), typescript, mandreel
// expect: 3628800,function,undefined
var fact = function f(n) { return n <= 1 ? 1 : n * f(n - 1); };
var o = { m: function self() { return typeof self; } };
[fact(10), o.m(), typeof f].join(",");
