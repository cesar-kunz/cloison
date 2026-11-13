// (cloison-fuzz triage/BUGS.md #26: each parameter was bound to its
// argument before any default ran. V8, JSC and SpiderMonkey agree.)
// expect: ReferenceError,4,ReferenceError,1,ReferenceError,ReferenceError,ReferenceError,ReferenceError,ReferenceError,false,ReferenceError,3,3,ReferenceError,5,7,ReferenceError,1,1,ReferenceError,9,ReferenceError,1,2,4,20000,20000

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => (function (x = x) { return x; })());
t(() => (function (x = x) { return x; })(4));
t(() => (function (x = y, y) { return x; })());
t(() => (function (x = y, y) { return x; })(1));
t(() => ((x = y, y) => x)());
t(() => (function* (a = b, b) {})());
t(() => (function (a = typeof b, b) { return a; })());
t(() => (function (a = (b = 1), b) { return a; })());
t(() => (function (a = (b++, 1), b) { return a; })());
t(() => (function (a = delete b, b) { return a; })());
t(() => (function (a = eval("b"), b) { return a; })(undefined, 3));
t(() => (function (a = eval("1"), b) { return b; })(undefined, 3));
t(() => (function (a = () => eval("b"), b) { return a(); })(undefined, 3));
t(() => (function (a = (() => b)(), b = 5) { return a; })());
t(() => (function (a = () => b, b) { return a; })(undefined, 5)());
t(() => (function (a = class { m() { return b; } }, b) { return new a().m(); })(undefined, 7));
t(() => (function (a = class { static s = b; }, b) { return a.s; })());
t(() => (function (a = function (b) { return b; }, b = 2) { return a(1); })());
t(() => (function (x = a, { a }) { return x; })(1, { a: 2 }));
t(() => (function ({ a = b, b }) { return a; })({}));
t(() => (function ([a, b = a]) { return b; })([9]));
t(() => (function (x = a, ...[a]) { return x; })());
t(() => (function ({ a } = { a: 1 }, b = a) { return b; })());
t(() => (function ([a], b = a) { return b; })([2]));
t(() => (function (a, b = a, c = b + a) { return c; })(2));
var cnt = 0, ok = 0;
function h(x = y, y) { return x; }
function m(a = () => b, b) { return a(); }
for (var i = 0; i < 20000; i++) { try { h(); } catch (e) { cnt++; } ok += m(undefined, 1); }
r.push(cnt, ok);
r.join(",");
