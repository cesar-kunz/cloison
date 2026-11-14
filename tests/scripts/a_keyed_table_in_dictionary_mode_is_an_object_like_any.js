// A plain object used as a table, getting keys made at run time by keyed
// stores, goes to dictionary mode at its 129th (it made a shape for each).
// Nothing tells: sites that read and wrote it by name and by key before
// see its properties after, hot in compiled code, with other objects of
// the shape it had; its keys keep their order through deletes, keys
// added again and attributes changed; accessors, `JSON.stringify`,
// `for-in`, `Object.keys`, sealing and freezing, its use as a prototype
// and of one, a symbol's and an index's key, and a class instance or a
// literal getting as many, are what V8, SpiderMonkey and JavaScriptCore
// give.
// expect: 4480 1740 8960 13460 36020 36040 300 k0,k1,k2 k297,k298,k299 44850 3896 300 297 k1,k3,k4 k298,k299,k0 again 298 g 299 298 7  1486 1486 true 299 k5 [5,"k5"] 299 300 true false true true 1006 1 200 200 19900 19900 b0,b1 inherited,b0 199 true true false 201 202 200 false true 0,1,k0,k1 Symbol(s) 303 150 11175 f0,f1 f148,f149 150:150:11175 400 a,b,t0 79800 3
var out = [];
function table(n, from) {
  var o = from || {};
  for (var i = 0; i < n; i++) o["k" + i] = i;
  return o;
}
function named(o) { return o.k5 + o.k100 + o.k119; }
function keyed(o, a, b) { return o[a] + o[b]; }
function store(o, v) { o.k7 = v; o["k" + 8] = v + 1; return o.k7 + o.k8; }
function sum(o) { var s = 0; for (var k in o) s += o[k]; return s; }
function hot(f, a, b, c) { var s = 0; for (var i = 0; i < 20; i++) s += f(a, b, c); return s; }

// Read and written before the switch, by two objects of one shape, and
// after it, by the one that went on and the one that didn't.
var o = table(120), twin = table(120);
out.push(hot(named, o) + hot(named, twin) - hot(named, twin), hot(keyed, o, "k3", "k84"));
table(300, o);
out.push(hot(named, o) + hot(named, twin), hot(keyed, o, "k3", "k84") + hot(keyed, twin, "k3", "k84") + hot(keyed, o, "k200", "k299"));
out.push(hot(store, o, 900) + hot(store, twin, 10) - 420, hot(store, twin, 10) + hot(store, o, 900) - 400);
o.k7 = 7; o.k8 = 8;
out.push(Object.keys(o).length, Object.keys(o).slice(0, 3).join(), Object.keys(o).slice(-3).join(), sum(o), hot(sum, twin) / 20 - 3250);

// Deletes, a key added again, a redefinition, an accessor.
out.push(JSON.stringify(o).split(",").length);
delete o.k0; delete o.k2; delete o["k" + 150];
out.push(Object.keys(o).length, Object.keys(o).slice(0, 3).join());
o.k0 = "again";
out.push(Object.keys(o).slice(-3).join(), o.k0, Object.keys(o).length);
Object.defineProperty(o, "k1", { value: 1, enumerable: false, writable: false, configurable: true });
Object.defineProperty(o, "g", { get: function () { return this.k3 + this.k4; }, set: function (v) { this.k3 = v; }, enumerable: true, configurable: true });
out.push(Object.keys(o).slice(-1).join(), Object.getOwnPropertyNames(o).length + Object.getOwnPropertySymbols(o).length,
  Reflect.ownKeys(o).length + Object.keys(o).length - 299, o.g);
o.k1 = 50; o.g = 1000;
function getter(o) { return o.g + o.k1 + o.k300; }
o.k300 = 481;
out.push(o.k150, hot(getter, o) / 20, getter(o), JSON.stringify(o).indexOf('"g":1004') > 0);
var seen = [];
for (var k in o) seen.push(k);
out.push(seen.length, seen[2], JSON.stringify(Object.entries(o)[2].reverse()));

// Sealed, then frozen.
Object.seal(o);
o.k5 = 6; o.fresh = 1; delete o.k9;
out.push(Object.keys(o).length, Object.getOwnPropertyNames(o).length, Object.isSealed(o), Object.isFrozen(o));
Object.freeze(o);
o.k5 = 7; o.g = 5;
out.push(Object.isFrozen(o), "k9" in o, o.k5 + o.k3, o.k1);

// A prototype's use, of a table and by one.
var proto = table(100);
var child = Object.create(proto);
function inherited(c) { return c.k50 + c.k99 + (c.own | 0); }
var before = hot(inherited, child);
for (var i = 0; i < 200; i++) proto["p" + i] = 1;
child.own = 1;
out.push(Object.keys(proto).length - 100, sum(child) - sum(proto) + 199, hot(inherited, child) - before + 19880);
var base = { inherited: 1 };
var t = Object.create(base);
for (var i = 0; i < 200; i++) t["b" + i] = i;
function over(t) { return t.inherited + t.b199 + t.b0; }
out.push(sum(t) - 1 + hot(over, t) - 4000, Object.keys(t).slice(0, 2).join());
seen = [];
for (var k in t) seen.push(k);
base.b0 = "shadowed";
out.push(seen.slice(-1).concat(seen.slice(0, 1)).join(), t.b199, t.hasOwnProperty("b150"), Object.getPrototypeOf(t) === base, t.hasOwnProperty("inherited"), seen.length, sum(t) - 19699);
Object.setPrototypeOf(t, null);
out.push(Object.keys(t).length, "inherited" in t, "b7" in t);

// Indices and a symbol among the keys.
var sym = Symbol("s");
var m = table(300);
m[1] = "one"; m[sym] = "sym"; m[0] = "zero";
out.push(Object.keys(m).slice(0, 4).join(), String(Object.getOwnPropertySymbols(m)[0]), Reflect.ownKeys(m).length);

// A class's instances and literals filled alike by keyed stores.
class Record { constructor(n) { for (var i = 0; i < n; i++) this["f" + i] = i; } total() { return sum(this); } }
var r1 = new Record(150), r2 = new Record(150), r3 = new Record(150);
out.push(Object.keys(r2).length, r1.total() + r2.total() - r3.total(), Object.keys(r3).slice(0, 2).join(), Object.keys(r3).slice(-2).join());
function fields(r) { return r.f0 + r.f149 + r.f75; }
out.push([hot(fields, r1) / 20 - 74, Object.keys(r1).length, r2.total()].join(":"));
var lit = { a: 1, b: 2 };
for (var i = 0; i < 400; i++) lit["t" + i] = i;
lit.a = 3;
out.push(Object.keys(lit).length - 2, Object.keys(lit).slice(0, 3).join(), sum(lit) - 5, lit.a);
out.join(" ")
