// A symbol gets its key atom the first time it is a key (most never
// are, and those that are are kept for good): its properties, their
// order, `getOwnPropertySymbols`, `Symbol.for`, collections, classes,
// proxies and spreads, as V8, SpiderMonkey, JavaScriptCore and QuickJS.
// expect-json: "3 true true true Symbol(b) undefined\nx,Symbol(b),Symbol(a),Symbol()\ntrue false 2 undefined true false 2\nmd false\n5 false\ntrue reg undefined\n7 false 1 {\"x\":3}\n9 10 11\n300 true s7000\n2 1\n1 true\ntrue symbol 1"

var out = [];
var log = function () { out.push(Array.prototype.map.call(arguments, String).join(" ")); };
var a = Symbol("a"), b = Symbol("b"), c = Symbol(), d = Symbol("d");
var o = {};
o[b] = 1; o[a] = 2; o.x = 3; o[c] = 4;
var ks = Object.getOwnPropertySymbols(o);
log(ks.length, ks[0] === b, ks[1] === a, ks[2] === c, String(ks[0]), ks[2].description);
log(Reflect.ownKeys(o).map(String).join());
log(a in o, d in o, o[a], o[d], delete o[a], a in o, Object.getOwnPropertySymbols(o).length);
var m = new Map([[d, "md"]]); log(m.get(d), m.has(Symbol("d")));
var w = new WeakMap(); var k = Symbol("w"); w.set(k, 5); log(w.get(k), w.has(Symbol("w")));
var r = Symbol.for("reg"); log(Symbol.for("reg") === r, Symbol.keyFor(r), Symbol.keyFor(a));
var p = {}; Object.defineProperty(p, d, { value: 7, enumerable: false });
log(p[d], Object.getOwnPropertyDescriptor(p, d).enumerable, Object.assign({}, o)[b], JSON.stringify(o));
class C { [d]() { return 9 } static [a] = 10; #q = 11; q() { return this.#q } }
log(new C()[d](), C[a], new C().q());
var many = []; for (var i = 0; i < 300000; i++) { var s = Symbol("s" + i); if (i % 1000 == 0) many.push(s); }
var big = {}; many.forEach(function (s, i) { big[s] = i; });
var got = Object.getOwnPropertySymbols(big); log(got.length, got.every(function (s, i) { return s === many[i] && big[s] === i }), got[7].description);
var px = new Proxy(o, {}); log(Object.getOwnPropertySymbols(px).length, px[b]);
var e = Symbol("e"); var q = { [e]: 1 }; var q2 = { ...q }; log(q2[e], Object.getOwnPropertySymbols(q2)[0] === e);
log(Symbol.iterator in [], typeof Symbol.asyncIterator, [1,2][Symbol.iterator]().next().value);
out.join("\n")
