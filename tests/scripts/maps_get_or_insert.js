// `Map` and `WeakMap`'s `getOrInsert` and `getOrInsertComputed`.
// expect: a,c,c,z,true,outer,outer,4,a,0,5,5,5,TypeError,TypeError,false

var r = [];
var m = new Map([[1, "a"]]);
r.push(m.getOrInsert(1, "b"), m.getOrInsert(2, "c"), m.get(2), m.getOrInsert(-0, "z"), m.has(0));
r.push(m.getOrInsertComputed(3, function (k) { m.set(k, "inner"); return "outer"; }), m.get(3), m.size);
var calls = 0;
r.push(m.getOrInsertComputed(1, function () { calls++; }), calls);
var w = new WeakMap(), k = {};
r.push(w.getOrInsert(k, 5), w.getOrInsertComputed(k, function () { return 6; }), w.get(k));
try { w.getOrInsert(1, 2); } catch (e) { r.push(e.name); }
try { m.getOrInsertComputed(9, 0); } catch (e) { r.push(e.name, m.has(9)); }
r.join();
