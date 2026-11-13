// `Object.getOwnPropertyDescriptors`, `Object.defineProperties` and
// `Object.create`'s second argument read their argument's own properties
// through its internal methods: a proxy's traps (a revoked one throws),
// and symbol keys too.
// expect: {"a":{"value":1,"writable":true,"enumerable":true,"configurable":true}} {"a":1} {"a":2} 1 3 1 TypeError TypeError keys,gopd:a,keys,gopd:a

var log = [];
var h = { ownKeys(t) { log.push("keys"); return Reflect.ownKeys(t); },
  getOwnPropertyDescriptor(t, k) { log.push("gopd:" + String(k)); return Reflect.getOwnPropertyDescriptor(t, k); } };
var r = [JSON.stringify(Object.getOwnPropertyDescriptors(new Proxy({ a: 1 }, h)))];
r.push(JSON.stringify(Object.defineProperties({}, new Proxy({ a: { value: 1, enumerable: true } }, h))));
r.push(JSON.stringify(Object.create(null, new Proxy({ a: { value: 2, enumerable: true } }, {}))));
var s = Symbol();
r.push(Object.defineProperties({}, { [s]: { value: 1 } })[s], Object.create(null, { [s]: { value: 3 } })[s]);
r.push(Object.getOwnPropertyDescriptors({ [s]: 1 })[s].value);
var rev = Proxy.revocable({}, {}); rev.revoke();
for (var f of [() => Object.defineProperties({}, rev.proxy), () => Object.getOwnPropertyDescriptors(rev.proxy)])
  try { f(); r.push("no throw"); } catch (e) { r.push(e.constructor.name); }
r.push(log.join());
r.join(" ");
