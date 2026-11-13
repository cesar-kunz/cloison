// A direct `eval`'s declarations are checked against every binding of
// their names between it and its function's scope, not only the
// innermost one, which its code's names find: a simple catch
// parameter (B.3.4's exception) in a block with a `let` by its name
// doesn't hide the `let`, so a `var` is a SyntaxError and a block
// function makes no `var` (19.2.1.3 steps 3.d and 11). For code
// compiled for its site's bindings or with a scope object (`// eval`),
// in a `with`, from a nested `eval`, a catch with a pattern (no
// exception), a `let` inside a catch, two catches, 3000 times.
// SpiderMonkey's, JSC's and V8's values.
// expect: undefined SyntaxError SyntaxError undefined SyntaxError undefined SyntaxError undefined SyntaxError SyntaxError undefined SyntaxError undefined 1 1 number arrow SyntaxError SyntaxError

var log = [];
function t(f) { try { log.push(String(f())); } catch (e) { log.push(e.name); } }
t(() => (function () { { let x; try { throw 1; } catch (x) { eval("{ function x() {} }"); } } return typeof x; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { eval("var x"); } } return "no"; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { eval("var x; // eval"); } } return "no"; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { eval("{ function x() {} } // eval"); } } return typeof x; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { with ({}) eval("var x"); } } return "no"; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { with ({ x: 1 }) eval("{ function x() {} }"); } } return typeof x; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { eval('eval("var x")'); } } return "no"; })());
t(() => (function () { { let x; try { throw 1; } catch (x) { eval('eval("{ function x() {} }")'); } } return typeof x; })());
t(() => (function () { { let x; try { throw [1]; } catch ([x]) { eval("var x"); } } return "no"; })());
t(() => (function () { try { throw [1]; } catch ([x]) { eval("var x"); } return "no"; })());
t(() => (function () { try { throw [1]; } catch ([x]) { eval("{ function x() {} }"); } return typeof x; })());
t(() => (function () { try { throw 1; } catch (x) { { let x; eval("var x"); } } return "no"; })());
t(() => (function () { try { throw 1; } catch (x) { { let x; eval("{ function x() {} }"); } } return typeof x; })());
t(() => (function () { try { throw 1; } catch (x) { try { throw 2; } catch (x) { eval("var x = 3"); } return x; } })());
t(() => (function () { try { throw 1; } catch (x) { try { throw 2; } catch (x) { eval("{ function x() {} }"); } return x; } })());
t(() => (function () { { let x = 5; try { throw 1; } catch (x) { return eval("{ function x() {} } typeof x"); } } })());
t(() => (function () { { let x; try { throw 1; } catch (x) { (() => eval("var x"))(); } } return "arrow"; })());
t(() => (function () { var s = ""; for (var i = 0; i < 3000; i++) { let x; try { throw 1; } catch (x) { try { eval("var x"); } catch (e) { s = e.name; } } } return s; })());
{ let y; try { throw 1; } catch (y) { try { eval("var y"); log.push("no"); } catch (e) { log.push(e.name); } } }
log.join(" ");
