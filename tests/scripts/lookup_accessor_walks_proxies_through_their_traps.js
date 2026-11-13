// `__lookupGetter__` and `__lookupSetter__` walk the prototype chain
// through its internal methods: a proxy's traps (a revoked one throws).
// expect: true true function TypeError

var g = function () { return 1; };
var p = new Proxy({}, { getOwnPropertyDescriptor(t, k) { return k === "x" ? { get: g, configurable: true } : undefined; } });
var q = Object.create(new Proxy({}, { getPrototypeOf() { return { set y(v) {} }; } }));
var r = [p.__lookupGetter__("x") === g, Object.create(p).__lookupGetter__("x") === g, typeof q.__lookupSetter__("y")];
var rev = Proxy.revocable({}, {}); rev.revoke();
try { Object.prototype.__lookupSetter__.call(rev.proxy, "x"); r.push("no throw"); } catch (e) { r.push(e.constructor.name); }
r.join(" ");
