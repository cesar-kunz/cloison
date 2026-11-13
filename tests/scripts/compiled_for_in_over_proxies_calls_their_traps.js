// Compiled `for … in` over a proxy, or an object inheriting from one,
// gets its keys from its traps (the interpreter calls them), and what
// they throw is thrown there.
// expect: 2000 2000 3000 3000 trap z 6000 3000

var px = new Proxy({ a: 1, b: 2 }, { ownKeys() { return ["a"]; } });
function f(o, n) { var s = ""; for (var i = 0; i < n; i++) for (var k in o) s += k; return s.length; }
var r = [f(px, 2000), f(px, 2000)];
var log = 0, px2 = new Proxy({ x: 1 }, { ownKeys(t) { log++; return Reflect.ownKeys(t); } });
r.push(f(px2, 3000), log);
var thrower = new Proxy({}, { ownKeys() { throw new Error("trap"); } });
function g(o) { try { for (var k in o) return k; return "none"; } catch (e) { return e.message; } }
for (var i = 0; i < 3000; i++) g({ q: 1 });
r.push(g(thrower), g({ z: 1 }));
var inh = Object.create(new Proxy({ p: 1 }, {}));
function h(o, n) { var c = 0; for (var i = 0; i < n; i++) for (var k in o) c++; return c; }
r.push(h({ a: 1, b: 2 }, 3000), h(inh, 3000));
r.join(" ");
