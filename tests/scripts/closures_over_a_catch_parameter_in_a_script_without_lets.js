// In a script with no block-scoped declarations but catch parameters, a
// function in a catch clause closes over the parameter (14.15.3: a
// declarative environment of the clause's), not the enclosing function's
// binding of its name: none, a `var`, a parameter; at the top level, in
// a method, in a loop (an environment each). Its catch parameters were
// left unrenamed, the function's own variables, so these read the
// enclosing binding (or threw a ReferenceError). SpiderMonkey's and V8's
// values.
// expect: 1 2 3 4 5 6 012 7 8 9

var log = [];
function f() { try { throw 1; } catch (e) { return () => e; } }
function g() { var e = 9; try { throw 2; } catch (e) { return () => e; } }
function h(e) { try { throw 3; } catch (e) { return function () { return e; }; } }
try { throw 4; } catch (e) { var top = () => e; }
function n() { try { throw 5; } catch (e) { function q() { return (() => e)(); } return q; } }
var o = { m() { try { throw 6; } catch (e) { return [0].map(() => e)[0]; } } };
function m() { var r = []; for (var i = 0; i < 3; i++) { try { throw i; } catch (e) { r.push(() => e); } } return r.map(x => x()).join(""); }
function a() { var e = 0; try { throw 7; } catch (e) { return { get p() { return e; } }.p; } }
function b() { try { throw 8; } catch (e) { return (function* () { yield e; })().next().value; } }
function c() { try { throw 9; } catch (e) { return ((x = e) => x)(); } }
log.push(f()(), g()(), h(7)(), top(), n()(), o.m(), m(), a(), b(), c());
log.join(" ");
