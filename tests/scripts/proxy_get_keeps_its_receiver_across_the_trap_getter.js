// A get through a proxy on the prototype chain holds the receiver across
// the handler's `get` getter: one only Rust refers to (the object an
// array's `constructor` getter makes, read for its species) stays alive
// for the trap it is given to.
// expect: c7|c7|c7
function junk() { var g = []; for (var i = 0; i < 20000; i++) g.push({ i }); return g.length; }
var seen = [];
var handler = {};
Object.defineProperty(handler, "get", { get() {
  junk();
  return function (t, k, r) { if (k === Symbol.species) seen.push(r.tag + r.more.x); return undefined; };
} });
var arr = [1, 2, 3];
Object.defineProperty(arr, "constructor", { get() {
  var c = Object.create(new Proxy({}, handler));
  c.tag = "c";
  c.more = { x: 7 };
  for (var i = 0; i < 30; i++) c["p" + i] = { i };
  return c;
} });
for (var k = 0; k < 3; k++) arr.map(function (x) { return x; });
seen.join("|");
