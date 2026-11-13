// A named function expression's own name stays immutable in a closure
// compiled lazily (`eval`'s code: its functions are stubs, their upvalues
// the parameters of `js::compile_lazy`'s wrapper): a store does nothing,
// in strict code throws.
// expect: function TypeError 2

var r = [];
eval("var g = function f() { return function () { f = 1; return typeof f; }; };");
r.push(g()());
eval("'use strict'; globalThis.h = function f() { return function () { return function () { try { f = 1; return 'no'; } catch (e) { return e.name; } }; }; };");
r.push(h()()());
eval("var k = function f() { var f; return function () { f = 2; return f; }; };");
r.push(k()());
r.join(" ");
