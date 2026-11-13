// A direct `eval` in a function (or arrow) of an `eval`'s code, or after
// a binding of that code's, sees the bindings around the outer `eval`
// too: the scopes of the outer call stay around the inner one's (ECMA-262
// 19.2.1.1 PerformEval, step 16: the eval code's environment is inside
// the caller's). Its assignments are theirs, a `const`'s a TypeError, a
// `var` it declares by a block-scoped binding's name between the
// function's scope and it a SyntaxError (19.2.1.3
// EvalDeclarationInstantiation), one it declares the inner function's,
// whose own `var`s shadow them. SpiderMonkey's values.
// expect: 1 2 3 4 5 7 7 8 9 10 11 12 TypeError 2 SyntaxError 1 14 14 15

var out = [];
function t(f) { try { out.push(String(f())); } catch (e) { out.push("!" + e.name); } }
t(() => (function() { let q = 1; return eval('(() => eval("q"))()'); })());
t(() => (function() { let q = 2; return eval('(function() { return eval("q"); })()'); })());
t(() => (function() { var q = 3; return eval('(function() { return eval("q"); })()'); })());
t(() => (function(a) { return eval('(function() { return eval("a"); })()'); })(4));
t(() => (function() { let q = 1; eval('(() => eval("q = 5"))()'); return q; })());
t(() => (function() { let q = 6; return eval('let r = 1; (() => eval("q + r"))()'); })());
t(() => (function() { let q = 1; return eval('let r = 6; eval("q + r")'); })());
t(() => (function() { let q = 1; return eval('(function() { var q = 8; return eval("q"); })()'); })());
t(() => (function() { let q = 9; return eval('eval("(() => eval(\'q\'))()")'); })());
t(() => (function() { let q = 10; with ({}) { return eval('(() => eval("q"))()'); } })());
t(() => (function() { let a = 1; return eval('with ({a: 11}) { (() => eval("a")); }')(); })());
t(() => (function() { "use strict"; let a = 12; return eval('(() => eval("a"))()'); })());
t(() => (function() { const c = 1; try { eval('(() => eval("c = 2"))()'); } catch (e) { return e.name; } })());
t(() => (function() { let a = 1; return eval('(function() { eval("var a = 2"); return a; })()'); })());
t(() => (function() { let a = 1; try { return eval('eval("var a = 2")'); } catch (e) { return e.name; } })());
t(() => (function() { var a = 1; eval('(() => eval("var a = 13"))()'); return a; })());
t(() => (function() { var a = 14; eval('(function() { eval("var a = 2"); })()'); return a; })());
t(() => (function(x) { return eval('(() => { x = 7; return eval("x"); })()') + x; })(1));
let gq = 15;
t(() => eval('(() => eval("gq"))()'));
out.join(" ");
