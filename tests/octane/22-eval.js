// Indirect (global) eval: declaring globals and functions, and its value.
// for: zlib (its asm.js code), code-load (1700 calls), earley-boyer
// expect: 3,6,42,function
var geval = eval;
geval("var fromEval = 6; function evalFn(x) { return x * fromEval; }");
var v = geval("1 + 2");
[v, fromEval, evalFn(7), typeof geval("(function () { return 1; })")].join(",");
