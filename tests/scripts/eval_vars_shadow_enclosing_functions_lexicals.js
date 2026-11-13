// A `var` a direct `eval` declares in a function shadows an enclosing
// function's `let` (no conflict: that one is past the function's own
// scope), as it does an enclosing function's `var`; one of the
// function's own blocks' still conflicts.
// expect: EL SyntaxError 6 TypeError 7

var r = [];
r.push((function () { let x = "L"; function f() { eval("var x='E'"); return x; } return f() + x; })());
r.push((function () { let x = "L"; try { eval("var x='E'"); } catch (e) { return e.name; } return x; })());
r.push((function () { const y = 1; function g() { eval("var y = 5"); return y; } return g() + y; })());
r.push((function () { const y = 1; function g() { try { eval("y = 2"); } catch (e) { return e.name; } } return g(); })());
r.push((function () { let w = 1; function g() { eval("w = 7"); } g(); return w; })());
r.join(" ");
