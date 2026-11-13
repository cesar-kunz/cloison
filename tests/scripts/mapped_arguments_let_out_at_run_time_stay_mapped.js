// #163's mapped `arguments` objects that can't outlive their frame keep
// copying their parameters' values at its end (`codegen::arguments_escape`
// finds them; nothing can see the difference), but one let out at run
// time is kept mapped: through a prototype's getter or setter that gets
// it as `this` (for an index past it, a name as a key, `__proto__` made
// a proxy whose traps get it), a method of it called (plainly or in an
// optional chain), an arrow reading it, a conversion of it, an `apply`
// that isn't the builtin. What
// SpiderMonkey and JavaScriptCore give.
// expect: 5 6 5 7 5 8 5 9 5 10 5 11 5 12 5 13 3 5 14 2 1 true 5

var out = [];
var leaked;
// A prototype's getter for an index past the arguments gets them as `this`.
Object.defineProperty(Object.prototype, "7", { get: function () { leaked = this; return 0; }, configurable: true });
function g1(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; var x = arguments[7]; return k; }
var k1 = g1(1); k1(5); out.push(leaked[0]); leaked[0] = 6; out.push(k1());
delete Object.prototype[7];
// A prototype's setter for a name stored through a key.
Object.defineProperty(Object.prototype, "foo", { set: function (v) { leaked = this; }, configurable: true });
function g2(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; arguments["foo"] = 1; return k; }
var k2 = g2(1); k2(5); out.push(leaked[0]); leaked[0] = 7; out.push(k2());
delete Object.prototype.foo;
// A method of it, plainly and through an optional chain.
function g3(a, f) { var k = function (v) { if (v !== undefined) a = v; return a; }; arguments[1](); return k; }
var k3 = g3(1, function () { leaked = this; }); k3(5); out.push(leaked[0]); leaked[0] = 8; out.push(k3());
function g4(a, f) { var k = function (v) { if (v !== undefined) a = v; return a; }; arguments?.[1](); return k; }
var k4 = g4(1, function () { leaked = this; }); k4(5); out.push(leaked[0]); leaked[0] = 9; out.push(k4());
// An arrow reading it, a conversion of it.
function g5(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; var get = () => arguments; return [k, get]; }
var r5 = g5(1); r5[0](5); out.push(r5[1]()[0]); r5[1]()[0] = 10; out.push(r5[0]());
function g6(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; var o = { toString: function () { return "" + arguments; } }; "" + arguments; return k; }
var save = Object.prototype.toString;
Object.prototype.toString = function () { leaked = this; return "x"; };
var k6 = g6(1);
Object.prototype.toString = save;
k6(5); out.push(leaked[0]); leaked[0] = 11; out.push(k6());
// Its prototype made a proxy through a key: the proxy's traps get it.
function g7(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; arguments["__proto__"] = new Proxy({}, { get: function (t, key, r) { leaked = r; return 1; } }); arguments["bar"]; return k; }
var k7 = g7(1); k7(5); out.push(leaked[0]); leaked[0] = 12; out.push(k7());
// An `apply` of a function's own, which keeps it (not the builtin's
// copy); and the builtin's, forwarding it.
function t9() {}
t9.apply = function (_, args) { leaked = args; };
function g9(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; t9.apply(null, arguments); return k; }
var k9 = g9(1); k9(5); out.push(leaked[0]); leaked[0] = 13; out.push(k9());
function g10(a) { var k = function (v) { if (v !== undefined) a = v; return a; }; return [k, Math.max.apply(null, arguments)]; }
var r10 = g10(3); r10[0](5); out.push(r10[1], r10[0]());
// While it runs, too: what that `apply` writes is the parameter's.
t9.apply = function (_, args) { args[0] = 14; };
function g11(a) { t9.apply(null, arguments); return a; }
out.push(g11(1));
// Read only: nothing to see after the return, the closure as before.
function g8(a, b) { var k = function (v) { if (v !== undefined) a = v; return a; }; return [k, arguments.length, arguments[0], arguments.callee === g8]; }
var r8 = g8(1, 2); r8[0](5); out.push(r8[1], r8[2], r8[3], r8[0]());
out.join(" ")
