// A proxy on the global object's prototype chain answers whether a
// global exists (its `has`), and takes stores to globals the global
// object hasn't (its `set`). A strict store to one it says doesn't exist
// is a ReferenceError, a sloppy one goes to `set` (the spec's PutValue;
// V8 skips `has` for stores, QuickJS throws on the sloppy one).
// expect: 42 number undefined ReferenceError ReferenceError false false has zz,get zz,has zq,set zs=1,set zn=2,has zs,set zs=3,has zm

var log = [];
var P = new Proxy(Object.getPrototypeOf(globalThis), {
  has(t, k) { if (k[0] == "z") log.push("has " + k); return k === "zz" || k === "zs" || k in t; },
  get(t, k, r) { if (k[0] == "z") log.push("get " + k); return k === "zz" ? 42 : Reflect.get(t, k, r); },
  set(t, k, v, r) { log.push("set " + k + "=" + v); return true; },
});
Object.setPrototypeOf(globalThis, P);
var r = [zz, typeof zz, typeof zq];
try { zq; } catch (e) { r.push(e.name); }
zs = 1; zn = 2;
(function () { "use strict"; zs = 3; try { zm = 4; } catch (e) { r.push(e.name); } })();
r.push(Object.hasOwn(globalThis, "zs"), Object.hasOwn(globalThis, "zn"), [...new Set(log)].join());
r.join(" ");
