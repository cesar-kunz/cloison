// A `var` a direct `eval` declares in a function shadows an enclosing
// function's binding by its name, for that `eval` and the next ones,
// while a reference resolved before the `eval` still stores to the
// enclosing binding (test262's S11.13.1_A6_T2 and S11.13.2_A6.*: V8
// differs).
// expect: 2,1,7,5,9,5,3,11,6

var r = [];
function t1() { var x = 0; var inner = (function () { x = (eval("var x = 2;"), 1); return x; })(); r.push(inner, x); }
t1();
function t2() { var y = 5; (function () { eval("var y = 7"); r.push(y); })(); r.push(y); }
t2();
function t3() { var y = 5; (function () { eval("var y"); eval("y = 9"); r.push(y); })(); r.push(y); }
t3();
function t4() { var z = 1; (function () { z += (eval("var z = 3"), 10); r.push(z); })(); r.push(z); }
t4();
function t5() { var w = 5; (function () { eval("w = 6"); })(); r.push(w); }
t5();
r.join();
