// An object literal's named key past a Proxy prototype is defined, not
// the proxy's [[Set]] (cloison-fuzz BUGS.md #15).
// expect: 1 1 w 0

var hits = 0;
var ro = {}; Object.defineProperty(ro, "w", { value: 37, writable: false, configurable: true });
var px = new Proxy({}, { set() { hits += 100; return true; }, get(t, k) { return "P"; } });
[({ __proto__: px, w: 1 }).w, ({ __proto__: new Proxy(ro, {}), w: 1 }).w,
  Object.getOwnPropertyNames({ __proto__: px, w: 1 }).join(), hits].join(' ');
