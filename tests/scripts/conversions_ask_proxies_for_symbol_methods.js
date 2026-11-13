// ToPrimitive asks a proxy (or an object inheriting from one) for
// `Symbol.toPrimitive`, and `instanceof` a proxy for `Symbol.hasInstance`,
// though no object has had either.
// expect: 5 6 s true 10 1 6 TypeError true

var log = [];
var p = new Proxy({}, { get(t, k) { log.push(String(k)); return k === Symbol.toPrimitive ? h => h === "string" ? "s" : 5 : Reflect.get(t, k); } });
var r = [+p, p + 1, `${p}`, p == 5, Object.create(p) * 2, { [p]: 1 }.s, log.length];
var q = new Proxy({}, { get: (t, k) => k === Symbol.toPrimitive ? 1 : Reflect.get(t, k) });
try { r.push(q + ""); } catch (e) { r.push(e.constructor.name); }
var F = new Proxy(function () {}, { get: (t, k) => k === Symbol.hasInstance ? () => true : Reflect.get(t, k) });
r.push(1 instanceof F);
r.join(" ");
