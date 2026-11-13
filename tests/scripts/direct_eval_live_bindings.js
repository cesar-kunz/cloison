// Direct `eval`'s code reads and writes the bindings it sees themselves,
// not copies: block-scoped ones, an enclosing function's (from closures
// it makes too, after the call, and in a generator across `yield`s);
// a `const`'s assignment throws, one in its TDZ too, a `var` by a
// block-scoped binding's name is a SyntaxError; and the objects of the
// `with` statements around it come first.
// expect: 2,5,TypeError,SyntaxError,2,false,ReferenceError,012,2,8,11,9,9,2,7,2,400020000

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => { let x = 1; var g = eval("(function () { return x; })"); x = 2; return g(); });
t(() => { let x = 1; var g = eval("(function () { x = 5; })"); g(); return x; });
t(() => { const c = 1; eval("c = 2"); return c; });
t(() => { { let x = 1; eval("var x = 2"); return x; } });
t(() => { try { throw 1; } catch (e) { eval("var e = 2"); return e; } });
t(() => { let x = 1; return eval("delete x"); });
t(() => { return eval("typeof z"); let z; });
t(() => { var fs = []; for (let i = 0; i < 3; i++) fs.push(eval("() => i")); return fs.map(f => f()).join(""); });
t(() => { var k = 0; function f() { var g = eval("(function () { return ++k; })"); g(); g(); return k; } return f(); });
t(() => { let x = 1; function inner() { eval("x = 8"); } inner(); return x; });
t(() => { var x = 1; function inner() { var f = eval("() => x"); x = 11; return f(); } return inner(); });
t(() => { function* g() { let x = 1; var f = eval("() => x"); yield 0; x = 9; yield f(); } var it = g(); it.next(); return it.next().value; });
t(() => { function* g() { let x = 1; eval(""); var f = () => x; yield 0; x = 9; yield f(); } var it = g(); it.next(); return it.next().value; });
t(() => { var o = {p: 1}; with (o) { eval("p = 2"); } return o.p; });
t(() => { var p = 5, o = {p: 1}; with (o) { var f = eval("() => p"); } o.p = 7; return f(); });
t(() => { var o = {p: 1}, a = {p: 2}; with (o) with (a) { return eval("eval('p')"); } });
function hot() { let x = 0; const f = eval("(v) => { x += v; return x; }"); let s = 0; for (let i = 0; i < 20000; i++) s += f(1) + x; return s; }
t(hot);
r.join();
