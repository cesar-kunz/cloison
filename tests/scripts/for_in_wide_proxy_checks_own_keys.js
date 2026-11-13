// A wide proxy's `ownKeys` result, over a non-extensible target: the
// invariants still hold. (The result was checked for duplicates and
// against the target's keys by searching lists: O(n²).)
// expect: 3000 3000 TypeError TypeError

var n = 3000, t = {}, r = [], c = 0, k;
for (var i = 0; i < n; i++) Object.defineProperty(t, "k" + i, { value: i, enumerable: true });
Object.preventExtensions(t);
var p = new Proxy(t, { ownKeys(t) { return Reflect.ownKeys(t); } });
for (k in p) c++;
r.push(c, Reflect.ownKeys(p).length);
var q = new Proxy(t, { ownKeys(t) { return Reflect.ownKeys(t).slice(1); } });
try { Reflect.ownKeys(q); r.push("no throw"); } catch (e) { r.push(e.constructor.name); }
var u = new Proxy(t, { ownKeys(t) { return Reflect.ownKeys(t).concat("extra"); } });
try { Reflect.ownKeys(u); r.push("no throw"); } catch (e) { r.push(e.constructor.name); }
r.join(" ");
