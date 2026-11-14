// An object in dictionary mode (from its second delete) buries the slot
// of a key deleted, until half are: no listing of its keys shows one
// (names, symbols, `Reflect.ownKeys`, `for-in`, `JSON.stringify`, the
// descriptors, `Object.assign`, a spread), a key added again comes last,
// the last ones deleted free their slots, and what copies its shape
// (attributes changed, an accessor, a freeze, a prototype's use) has none
// buried. As V8, SpiderMonkey and JavaScriptCore.
// expect: k1,k3,k5,k7,k9,k11,k13,k15,k17,k19,k21,k23,k25,k27,k29,k31,k33,k35,k37,k39,x,k0 Symbol(b),Symbol(a) 24 190 22 k3,k5,k7,k9,k11,k13,k15,k17,k19,k21,k23,k25,k27,k29,k31,k33,k35,k37,k39,x,k0 21 4 k0,g k31,k33,k35,k37,k39,x,k0,g {"value":31,"writable":true,"enumerable":true,"configurable":true} true 8 true false 8 10 0 0 {} n1,n3,n5,n7,n9,n11,n13,n15,n17,n19,n0,n2,n4,n6,n8,n10,n12,n14,n16,n18 1,3,5,7,9,11,13,15,17,19,0,2,4,6,8,10,12,14,16,18 26 own,k1,k3 1  false true 25 25 a,c,e,f 3 a,c,e,b 2,4,6 6
var out = [];
function table(n) {
  var o = {};
  for (var i = 0; i < n; i++) o["k" + i] = i;
  return o;
}
var o = table(40);
var s1 = Symbol("a"), s2 = Symbol("b");
o[s1] = 1; o.x = 1; o[s2] = 2;
for (var i = 0; i < 40; i += 2) delete o["k" + i];
delete o[s1];
o.k0 = "again"; o[s1] = 3;
out.push(Object.keys(o).join(), Object.getOwnPropertySymbols(o).map(String).join(),
  Reflect.ownKeys(o).length, JSON.stringify(o).length, Object.getOwnPropertyNames(o).length);
Object.defineProperty(o, "k1", { enumerable: false });
out.push(Object.keys(o).join(), Object.entries(o).length);
Object.defineProperty(o, "g", { get() { return this.k3 + 1; }, enumerable: true, configurable: true });
out.push(o.g, Object.keys(o).slice(-2).join());
for (var i = 1; i < 30; i += 2) delete o["k" + i];
out.push(Object.keys(o).join(), JSON.stringify(Object.getOwnPropertyDescriptor(o, "k31")));
var c = Object.assign({}, o), d = { ...o };
out.push(Object.keys(c).join() == Object.keys(d).join(), Object.keys(c).length);
Object.freeze(o);
out.push(Object.isFrozen(o), delete o.k31, Object.keys(o).length, Reflect.ownKeys(o).length);
// All deleted, then filled again.
o = table(100);
for (var i = 0; i < 100; i++) delete o["k" + i];
out.push(Object.keys(o).length, Reflect.ownKeys(o).length, JSON.stringify(o));
for (var i = 0; i < 20; i++) o["n" + i] = i;
for (var i = 0; i < 20; i += 2) delete o["n" + i];
for (var i = 0; i < 20; i += 2) o["n" + i] = i;
out.push(Object.keys(o).join(), Object.values(o).join());
// Read through an object that inherits from it.
o = table(50);
for (var i = 0; i < 50; i += 2) delete o["k" + i];
var p = Object.create(o);
p.own = 1;
var seen = [];
for (var k in p) seen.push(k);
out.push(seen.length, seen.slice(0, 3).join(), p.k1, p.k0, o.hasOwnProperty("k0"), o.hasOwnProperty("k1"),
  Object.keys(o).length, Object.getOwnPropertyNames(o).length);
// The last keys deleted, those buried before them too; then sealed.
o = { a: 1, b: 2, c: 3, d: 4, e: 5, f: 6 };
delete o.b; delete o.d;
out.push(Object.keys(o).join());
delete o.f;
o.f = 6; delete o.f;
out.push(Object.keys(o).length);
o.b = 4;
Object.seal(o);
o.c = 4; o.e = 6; o.a = 2;
out.push(Object.keys(o).join(), [o.a, o.c, o.e].join(), o.b + o.a);
out.join(" ")
