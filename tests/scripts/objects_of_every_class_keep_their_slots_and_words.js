// Objects of each class after the 32-byte header: plain objects and
// arrays with no `code` word, their slots right after it, the other
// classes with `code` (and buffers', views', collection iterators' and
// BigInts' `aux`) first; capacities in the cells of `props` and `elems`,
// a far array's store in its elements' last slot as its dense prefix
// grows, shrinks and goes dense again; strings' atoms cached in their
// `code`. (What V8, SpiderMonkey, JavaScriptCore and QuickJS print.)
// expect: 302:598:300:299 | 1000057:xy:undefined:61:10:undefined:undefined:50:49:101:99 | 20803:204 | 15540:inner:0:7 | 4006000:020true:44m:boom1true | 4:32:7:1852:64:2,2.5,3,3.5q | 1071:43:k0,k2,k3:141,144,147:0,0:43:[object Map Iterator] | 1no b:7:-1180591620717411303419:true:1971:3b1:62:desc9:20 | 45:4,9,16

var r = [];
// Arrays growing: pushes past each capacity, a literal's room, appends.
function grow(n) { var a = [1, 2]; for (var i = 0; i < n; i++) a.push(i * 2); var b = []; for (var i = 0; i < n; i++) b[i] = i; return a.length + ":" + a[n + 1] + ":" + b.length + ":" + b[n - 1]; }
var g = ""; for (var k = 0; k < 300; k++) g = grow(k + 1); r.push(g);
// Far arrays: sparse, the dense prefix growing after, back to dense, truncated.
function far(n) {
  var a = [0, 1, 2]; a[1e6] = "x"; a[5e5] = "y";
  for (var i = 3; i < n; i++) a.push(i);
  var s = a.length + ":" + a[1e6] + a[5e5] + ":" + a[n - 1] + ":" + Object.keys(a).length;
  a.length = 10; s += ":" + a.length + ":" + a[9] + ":" + a[1e6];
  for (var i = 10; i < 50; i++) a[i] = i;
  var b = []; b[100] = 1; for (var i = 0; i < 100; i++) b[i] = i;
  return s + ":" + a.length + ":" + a[49] + ":" + b.length + ":" + b.indexOf(99);
}
var f = ""; for (var k = 0; k < 40; k++) f = far(20 + k); r.push(f);
// Many properties: out of line, deleted, re-added.
function many(n) { var o = {}; for (var i = 0; i < n; i++) o["p" + i] = i; delete o.p3; o.p3 = "z"; var t = 0; for (var k in o) t += typeof o[k] == "number" ? o[k] : 100; return t + ":" + Object.keys(o).length; }
var m = ""; for (var k = 0; k < 200; k++) m = many(k + 5); r.push(m);
// Functions as objects: properties, prototype, closures' upvalues.
function mk(i) { var c = i; function inner() { return c++; } inner.tag = "t" + i; inner.prototype.k = i; inner.a = 1; inner.b = 2; inner.c = 3; return inner; }
var fs = []; for (var i = 0; i < 100; i++) fs.push(mk(i));
var ft = 0; for (var i = 0; i < 100; i++) ft += fs[i]() + fs[i]() + fs[i].prototype.k + fs[i].c + fs[i].tag.length;
r.push(ft + ":" + fs[7].name + ":" + fs[7].length + ":" + (new fs[7]()).k);
// Class instances, a subclass of Array and of Map, errors.
class P { constructor(x) { this.x = x; this.y = x + 1; } sum() { return this.x + this.y; } }
class Q extends P { constructor(x) { super(x); this.z = 3; } }
class L extends Array { first() { return this[0]; } }
class M extends Map { get size2() { return this.size * 2; } }
var ct = 0; for (var i = 0; i < 2000; i++) { var q = new Q(i); ct += q.sum() + q.z; }
var l = new L(); for (var i = 0; i < 20; i++) l.push(i);
var mm = new M([[1, 2], [3, 4]]); mm.tag = "m";
var e = new Error("boom"); e.extra = 1;
r.push(ct + ":" + l.first() + l.length + (l instanceof Array) + ":" + mm.size2 + mm.get(3) + mm.tag + ":" + e.message + e.extra + (e instanceof Error));
// Typed arrays: fixed and length-tracking views of a resizable buffer, a DataView.
var rb = new ArrayBuffer(8, { maxByteLength: 64 });
var fixed = new Uint8Array(rb, 0, 4), track = new Uint8Array(rb), dv = new DataView(rb);
rb.resize(32); track[20] = 7; dv.setUint16(0, 0x1234);
var u = new Float64Array(16); for (var i = 0; i < 16; i++) u[i] = i / 2; u.p = "q";
r.push(fixed.length + ":" + track.length + ":" + track[20] + ":" + fixed[0] + fixed[1] + ":" + rb.maxByteLength + ":" + u.subarray(4, 8).join() + u.p);
// Map and Set: iterators of each mode, deletion while iterating.
var mp = new Map(), st = new Set();
for (var i = 0; i < 50; i++) { mp.set("k" + i, i); st.add(i * 3); }
var it = mp.entries(), acc = 0, n = 0;
for (var [k, v] of it) { if (v % 7 == 0) mp.delete("k" + (v + 1)); acc += v; n++; }
var ks = [...mp.keys()].slice(0, 3).join(), vs = [...st.values()].slice(-3).join();
var se = st.entries().next().value;
r.push(acc + ":" + n + ":" + ks + ":" + vs + ":" + se + ":" + mp.size + ":" + Object.prototype.toString.call(mp.values()));
// Proxies, BigInts, dates, wrappers, symbols, keys that are cons strings.
var px = new Proxy({ a: 1 }, { get(t, k) { return k in t ? t[k] : "no " + String(k); } });
var fp = new Proxy(function (x) { return x * 2; }, { apply(t, th, a) { return t(...a) + 1; } });
var big = -(2n ** 70n) + 5n, d = new Date(86400000 * 365), w = new String("abc"), nw = new Number(5); w.extra = 1; nw.k = 2;
var sym = Symbol("desc"), so = { [sym]: 9 };
var keyed = {}, base = "abcdefghijklmnopqrstuvwxyz";
for (var i = 0; i < 100; i++) { var key = base + (i % 5); keyed[key] = (keyed[key] | 0) + 1; }
r.push(px.a + px.b + ":" + fp(3) + ":" + big + ":" + (big < 0n) + ":" + d.getUTCFullYear() + ":" + w.length + w[1] + w.extra + ":" + (nw + 1) + nw.k + ":" + sym.description + so[sym] + ":" + keyed[base + "3"]);
// Generators, promises and iterator helpers keep their state.
function* gen() { var i = 0; while (true) yield i++; }
var gn = gen(), gs = 0; for (var i = 0; i < 10; i++) gs += gn.next().value;
var log = []; Promise.resolve(1).then(v => log.push(v));
r.push(gs + ":" + [1, 2, 3, 4].values().map(x => x * x).filter(x => x > 1).toArray().join());
r.join(" | ");
