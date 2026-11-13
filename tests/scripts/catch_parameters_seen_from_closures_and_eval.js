// A catch parameter is renamed apart only when something needs it: a
// function in the clause that refers to it (a class's field or static
// block, a getter, a generator, a closure assigning it, a function
// nested deeper or compiled lazily), or a direct `eval` in the clause.
// A function in the clause that doesn't refer to it, or an `eval`
// elsewhere, leaves the script parsed once. SpiderMonkey's and V8's
// values.
// expect: 1 1 40 2 3 outer 5 9 number undefined 80 2

var log = [];
var e = "outer";
try { throw 1; } catch (e) { class C { x = e; static { log.push(e); } } log.push(new C().x); }
try { throw 2; } catch (e) { var o = { get p() { return e; } }; }
try { throw 3; } catch (e) { var gen = function* () { yield e; }; }
try { throw 4; } catch (e) { var set = function () { e = 40; }; set(); log.push(e); }
try { throw 5; } catch (e) { var deep = function () { return function () { return e; }; }; }
try { throw 6; } catch (e) { var lazy = function () { var t = 0; for (var i = 0; i < 3; i++) t += i; return e + t; }; }
try { throw 7; } catch (x) { var typed = function () { return typeof x; }; var none = function () { return typeof y; }; }
function ev() { try { throw 8; } catch (e) { eval("var e = 80"); return e; } }
function ev2() { var e2 = 1; eval("e2 = 2"); return e2; }
log.push(o.p, gen().next().value, e, deep()(), lazy(), typed(), none(), ev(), ev2());
log.join(" ");
