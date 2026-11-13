// A direct `eval`'s code looks a name up in the scopes around the call
// in their order: a `with`'s object outside the function, or outside a
// block-scoped binding's declaration, doesn't shadow the function's own
// `let`, `const`, `var`, parameter, catch parameter, nor the binding;
// one inside does (ECMA-262 9.1.2.1 GetIdentifierReference, through the
// running context's LexicalEnvironment, 19.2.1.1 PerformEval). The
// `var` it declares inside one is the function's, its initializer
// setting the object's property (14.3.2.1). SpiderMonkey's values.
// expect: 1 2 3 number 5 6 w 8 2 3 2,3 4 5 ,2 SyntaxError

var out = [];
function t(f) { try { out.push(String(f())); } catch (e) { out.push("!" + e.name); } }
with ({z1: 'w'}) t(() => (function() { let z1 = 1; return eval('z1'); })());
with ({z2: 'w'}) t(() => (function() { var z2 = 2; return eval('z2'); })());
with ({z3: 'w'}) t(() => (function(z3) { return eval('z3'); })(3));
with ({z4: 'w'}) t(() => (function() { const z4 = 4; return eval('typeof z4'); })());
with ({z5: 'w'}) t(() => (function() { try { throw 5; } catch (z5) { return eval('z5'); } })());
with ({z6: 'w'}) t(() => (() => { let z6 = 6; return eval('z6'); })());
with ({z7: 'w'}) t(() => (function() { return eval('z7'); })());
with ({z8: 'w'}) t(() => (function() { let z8 = 1; eval('z8 = 8'); return z8; })());
t(() => (function() { let a = 1; with ({a: 2}) { return eval('a'); } })());
t(() => (function() { with ({a: 2}) { let a = 3; return eval('a'); } })());
t(() => (function() { let a = 1; with ({a: 2}) { let b = 3; return eval('[a, b].join()'); } })());
t(() => (function() { var x = 1; with ({x: 4}) { return (function() { return eval('x'); })(); } })());
t(() => (function() { var x = 5; with ({}) { return (function() { return eval('x'); })(); } })());
var o = {v: 1};
t(() => (function() { with (o) { eval('var v = 2'); } return [v, o.v].join(); })());
t(() => (function() { let a = 1; with ({}) { try { eval('var a = 2'); } catch (e) { return e.name; } } })());
out.join(" ");
