// A named function expression's own name is immutable: assigning it
// does nothing, in strict code throws, from the function or a closure in
// it; a parameter or `var` of the name is a variable as any.
// expect: true TypeError 4 5 function function

var r = [];
var ref = function B() { B = 1; return B; }; r.push(ref() === ref);
r.push((function g() { "use strict"; try { g = 1; return "no"; } catch (e) { return e.name; } })());
r.push((function h() { var h = 3; h = 4; return h; })(), (function k(k) { k = 5; return k; })(1));
r.push((function m() { return (() => { m = 2; return typeof m; })(); })());
r.push((function z() { z += 1; z++; return typeof z; })());
r.join(" ");
