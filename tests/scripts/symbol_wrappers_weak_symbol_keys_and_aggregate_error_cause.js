// A symbol's wrapper is an object to `JSON.stringify`; a symbol not in
// the registry is a `WeakMap`'s or a `WeakSet`'s key; `AggregateError`
// takes its options' `cause`, read before its errors.
// expect: {} {"a":{},"b":true} 1 true TypeError 5 2 false cause+it

var r = [JSON.stringify(Object(Symbol())), JSON.stringify({ a: Object(Symbol("x")), b: Object(true) })];
var wm = new WeakMap(), ws = new WeakSet(), s = Symbol("k");
wm.set(s, 1); ws.add(s); r.push(wm.get(s), ws.has(s));
try { wm.set(Symbol.for("reg"), 1); } catch (e) { r.push(e.name); }
var ae = new AggregateError([1, 2], "m", { cause: 5 });
r.push(ae.cause, ae.errors.length, Object.prototype.hasOwnProperty.call(new AggregateError([]), "cause"));
var log = [];
new AggregateError({ [Symbol.iterator]() { log.push("it"); return [][Symbol.iterator](); } }, undefined,
  { get cause() { log.push("cause"); return 1; } });
r.push(log.join("+"));
r.join(" ");
