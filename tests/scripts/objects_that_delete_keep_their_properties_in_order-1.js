// Objects deleting a property (a unique shape each) and others deleting
// again (dictionary mode, their shape changed in place, `dz`): order of
// keys after deletes and re-adds, symbols, many properties, accessors and
// attributes defined both ways, freezing, sealing and making not
// extensible after, `for-in` while deleting, one as a prototype later and
// a prototype's setter, a proxy over one, hot polymorphic reads and
// writes of them through fields read compiled, a constructor deleting
// and adding again, elements alongside, and one object's add/delete
// churn give what V8, SpiderMonkey, JavaScriptCore and QuickJS do, with
// the JIT or not and with the collector at every safepoint.
// expect: z,w,y,v|z,w,y,v|{"z":3,"w":4,"y":6,"v":7}|8|1|true|false|true|[["z",3],["w",4],["y",6],["v",7]]|{"z":3,"w":4,"y":6,"v":7}|{"z":3,"w":4,"y":6,"v":7}|40|1200|59||k1,k2,k4,k5,k7|15|5|42|false|true|p,g,nc|{"value":5,"writable":false,"enumerable":true,"configurable":true}|1||true|7||true||false|b,d|b,d,e|3|4||3|false|6|_v,v|c|3|385825562|25005000|b,a|0,1,5,b,c|zy3.5|5|15|449840|p23,p26,p29,p30

var out = [];
function keys(o) { var r = []; for (var k in o) r.push(k); return r.join(","); }
// Order after deletes and re-adds, own keys, symbols.
var s1 = Symbol("s");
var a = { x: 1, y: 2, z: 3, w: 4 }; a[s1] = 5; delete a.y; a.y = 6; delete a.x; a.v = 7; a[s1] = 8;
out.push(keys(a), Object.keys(a).join(), JSON.stringify(a), a[s1], Object.getOwnPropertySymbols(a).length, "y" in a, "x" in a, a.hasOwnProperty("w"));
out.push(JSON.stringify(Object.entries(a)), JSON.stringify({ ...a }), JSON.stringify(Object.assign({}, a)));
// Many properties (past the linear lookups), deleting from the middle.
var b = {}; for (var i = 0; i < 60; i++) b["k" + i] = i;
for (var i = 0; i < 60; i += 3) delete b["k" + i];
var sum = 0; for (var i = 0; i < 60; i++) sum += b["k" + i] | 0;
out.push(Object.keys(b).length, sum, b.k59, b.k57, Object.keys(b).slice(0, 5).join());
// Accessors, attributes, defineProperty both ways, non-configurable.
var c = { p: 1, q: 2, r: 3 }; delete c.q;
Object.defineProperty(c, "g", { get: function () { return this.p + 10; }, configurable: true, enumerable: true });
Object.defineProperty(c, "p", { value: 5, writable: false });
c.p = 9; var t = c.g;
Object.defineProperty(c, "g", { value: 42, writable: true });
Object.defineProperty(c, "nc", { value: 1, configurable: false, enumerable: true });
out.push(t, c.p, c.g, delete c.nc, delete c.r, keys(c), JSON.stringify(Object.getOwnPropertyDescriptor(c, "p")));
// Freeze, seal, preventExtensions after deletes.
var d = { a: 1, b: 2, c: 3 }; delete d.b; Object.freeze(d); d.a = 5; d.n = 1;
var e = { a: 1, b: 2 }; delete e.a; Object.seal(e); e.b = 7; delete e.b; e.n = 1;
var f = { a: 1, b: 2 }; delete f.a; Object.preventExtensions(f); f.c = 3; delete f.b;
out.push(d.a, d.n, Object.isFrozen(d), e.b, e.n, Object.isSealed(e), keys(f), Object.isExtensible(f));
// for-in while deleting.
var g = { a: 1, b: 2, c: 3, d: 4 }; delete g.a; var seen = [];
for (var k in g) { seen.push(k); delete g.c; g.e = 5; }
out.push(seen.join(), keys(g));
// As a prototype later, and with a prototype setter.
var h = { m: 1, n: 2 }; delete h.m; h.o = 3;
var child = Object.create(h); var child2 = Object.setPrototypeOf({ z: 1 }, h);
h.p = 4; delete h.n;
out.push(child.o, child.p, child.n, child2.o, "n" in child2);
var proto = { set v(x) { this._v = x * 2; } };
var r = Object.create(proto); r.a = 1; delete r.a; r.v = 3; r.w = 1; delete r.w;
out.push(r._v, keys(r));
// A proxy over one.
var pr = { a: 1, b: 2 }; delete pr.a;
var px = new Proxy(pr, {}); px.c = 3; delete px.b;
out.push(keys(pr), px.c);
// Hot code: polymorphic reads and writes of objects in dictionary mode,
// stored into fields and read back compiled.
function mk(i) { var o = { a: i, b: i + 1, c: i + 2, d: i + 3 }; if (i % 3) delete o.b; if (i % 5 == 0) delete o.a; return o; }
function rd(o) { return (o.a | 0) + (o.b | 0) * 2 + (o.c | 0) * 3 + (o.d | 0) * 4; }
function holder(o) { this.inner = o; this.k = 1; }
var acc = 0;
for (var i = 0; i < 20000; i++) {
  var o = mk(i); var hd = new holder(o);
  acc = (acc + rd(o) + rd(hd.inner)) | 0;
  o.e = i; delete o.c; acc = (acc + (o.e | 0) + rd(o) + (hd.inner.c | 0)) | 0;
  if (i % 7 == 0) { o.c = 1; acc = (acc + hd.inner.c) | 0; }
}
out.push(acc);
// A constructor deleting in itself, and re-adding the same name.
function D(i) { this.a = i; this.b = 2; delete this.a; this.a = i * 2; }
var s2 = 0; for (var i = 0; i < 5000; i++) { var x = new D(i); s2 = (s2 + x.a + x.b) | 0; }
out.push(s2, keys(new D(1)));
// Elements alongside named properties.
var el = { a: 1, 0: "z", b: 2 }; delete el.a; el[1] = "y"; el[5] = 3.5; el.c = {};
out.push(keys(el), el[0] + el[1] + el[5], Object.keys(el).length);
// The add/delete churn of one object (del.js).
var q = {};
for (var i = 0; i < 30000; i++) { q["p" + (i & 31)] = i; delete q["p" + ((i * 7) & 31)]; }
out.push(Object.keys(q).length, Object.values(q).reduce(function (x, y) { return x + y; }, 0), Object.keys(q).slice(0, 4).join());
out.join("|")
