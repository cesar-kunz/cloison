// `Reflect.set` with a proxy receiver asks its traps: the
// `getOwnPropertyDescriptor` it reports (read-only: no store), and the
// `defineProperty` the store makes.
// expect: false | gopd:a | true | def:{"value":4} | 4

var r = [], log = [];
var rcv = new Proxy({}, { getOwnPropertyDescriptor(t, k) { log.push("gopd:" + k); return { value: 1, writable: false, configurable: true }; },
  defineProperty(t, k, d) { log.push("def:" + k); return Reflect.defineProperty(t, k, d); } });
r.push(Reflect.set({}, "a", 2, rcv), log.join());
log = []; var t3 = { c: 0 }; var rcv3 = new Proxy(t3, { defineProperty(t, k, d) { log.push("def:" + JSON.stringify(d)); return Reflect.defineProperty(t, k, d); } });
r.push(Reflect.set({}, "c", 4, rcv3), log.join(), t3.c);
r.join(" | ");
