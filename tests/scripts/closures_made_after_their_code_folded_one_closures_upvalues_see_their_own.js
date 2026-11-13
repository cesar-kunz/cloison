// A function compiled on its first call's loop calls an arrow it makes
// (capturing an object, or `arguments`): the arrow's code, compiled
// while it had one closure, took that closure's upvalues as constants,
// and the call bound to that code (any closure of it) kept calling it
// for the next calls' arrows, which read the first call's object. A
// second closure now unbinds those calls too: what V8, SpiderMonkey,
// JavaScriptCore and QuickJS give. (cloison-fuzz's C02.)
// expect: 1 2 3|1 2 3|1 2 3|1 2 3|1 2 3|1 2 3|1 2 3|2999

var out = [];
function f(a) {
  for (let k = 0; k < 100000; k++) {}
  return (() => arguments[0])();
}
function g(a) {
  const c = { v: a };
  for (let k = 0; k < 100000; k++) {}
  return (() => c.v)();
}
out.push([f(1), f(2), f(3)].join(" "));
out.push([g(1), g(2), g(3)].join(" "));
function g1(a) { const c = { v: a }; for (let k = 0; k < 100000; k++) {} return (() => c.v)(); }
function g2(a) { const c = { v: a }; for (let k = 0; k < 100000; k++) {} var h = () => c.v; return h; }
function g3(a) { const c = { v: a }; for (let k = 0; k < 100000; k++) {} return c.v; }
function g4(a) { let c = a; for (let k = 0; k < 100000; k++) {} return (() => c)(); }
function g5(a) { const c = { v: a }; for (let k = 0; k < 100000; k++) {} var h = () => c.v; return h(); }
function g6(a) { const c = { v: a }; for (let k = 0; k < 10; k++) {} return (() => c.v)(); }
out.push([g1(1), g1(2), g1(3)].join(" "));
out.push([g2(1)(), g2(2)(), g2(3)()].join(" "));
out.push([g3(1), g3(2), g3(3)].join(" "));
out.push([g4(1), g4(2), g4(3)].join(" "));
out.push([g5(1), g5(2), g5(3)].join(" "));
var s = ""; for (var i = 0; i < 3000; i++) s = g6(i); out.push(s);

out.join("|")
