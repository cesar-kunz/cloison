// `delete x` in a direct `eval` leaves the function's own bindings (its
// parameters, `var`s and functions), deleting only what an `eval`
// declared.
// expect: false,number false,number true,undefined false,function 2

var r = [];
function f() { var dd = 1; var res = eval("delete dd"); return res + "," + typeof dd; } r.push(f());
function g(p) { var res = eval("delete p"); return res + "," + typeof p; } r.push(g(1));
function h() { eval("var e = 1"); var res = eval("delete e"); return res + "," + typeof e; } r.push(h());
function k() { function inner() {} var res = eval("delete inner"); return res + "," + typeof inner; } r.push(k());
function v() { var w = 1; eval("var w = 2"); return w; } r.push(v());
r.join(" ");
