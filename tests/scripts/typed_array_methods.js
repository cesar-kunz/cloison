// `%TypedArray%` (abstract, with `from` and `of`), the kinds'
// constructors and prototypes inheriting it, and its prototype's
// methods: the Array ones, `sort`, the copying ones, `Symbol.toStringTag`.
// expect: shape function,TypedArray,true,true | abstract !TypeError | callAbstract !TypeError | map Int16Array:5000,-3000,10000,0,7000 | filter 5,10,7 | slice -3,10,0|0,7 | sort -Infinity,0,0,1,3,NaN|true | sortCmp 3,2,1 | toSorted -3,0,5,7,10|5,-3,10,0,7 | toReversed 7,0,10,-3,5 | with 5,-3,10,0,99|300,-3,10,0,7 | withRange !RangeError | generic 7,2,3,true,-3,3,7,true,true,19,7010-35 | reverse 3,2,1 | copyWithin 4,5,3,4,5 | forEach 19 | from 1,2,44|1.5,2|8,10 | of 1,2,4464 | tag [object Int16Array],Int16Array, | badThis !TypeError | badThis2 !TypeError | locale 1,2 | subclass true:1,2

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
function t(name, f) { try { log(name, f()); } catch (e) { log(name, "!" + e.name); } }
var TA = Object.getPrototypeOf(Int8Array);
t("shape", () => [typeof TA, TA.name, Object.getPrototypeOf(Float64Array.prototype) === TA.prototype, Object.getPrototypeOf(TA.prototype) === Object.prototype].join());
t("abstract", () => new TA());
t("callAbstract", () => TA());
var a = new Int16Array([5, -3, 10, 0, 7]);
t("map", () => { var m = a.map(x => x * 1000); return m.constructor.name + ":" + m.join(); });
t("filter", () => a.filter(x => x > 0).join());
t("slice", () => a.slice(1, -1).join() + "|" + a.slice(-2).join());
t("sort", () => new Float64Array([3, NaN, -0, 0, -Infinity, 1]).sort().join() + "|" + Object.is(new Float64Array([0, -0]).sort()[0], -0));
t("sortCmp", () => new Int8Array([1, 2, 3]).sort((x, y) => y - x).join());
t("toSorted", () => { var s = a.toSorted(); return s.join() + "|" + a.join(); });
t("toReversed", () => a.toReversed().join());
t("with", () => a.with(-1, 99).join() + "|" + a.with(0, 300).join());
t("withRange", () => a.with(5, 1));
t("generic", () => [a.at(-1), a.indexOf(10), a.lastIndexOf(0), a.includes(7), a.find(x => x < 0), a.findIndex(x => x == 0), a.findLast(x => x > 0), a.every(x => x > -5), a.some(x => x > 9), a.reduce((s, x) => s + x), a.reduceRight((s, x) => s + "" + x, "")].join());
t("reverse", () => new Uint8Array([1, 2, 3]).reverse().join());
t("copyWithin", () => new Uint8Array([1, 2, 3, 4, 5]).copyWithin(0, 3).join());
t("forEach", () => { var s = 0; a.forEach(x => s += x); return s; });
t("from", () => Int8Array.from([1, 2, 300]).join() + "|" + Float32Array.from({ length: 2, 0: 1.5, 1: 2 }).join() + "|" + Uint8Array.from(new Set([4, 5]), x => x * 2).join());
t("of", () => Uint16Array.of(1, 2, 70000).join());
t("tag", () => [Object.prototype.toString.call(a), a[Symbol.toStringTag], TA.prototype[Symbol.toStringTag]].join());
t("badThis", () => TA.prototype.map.call([1, 2], x => x));
t("badThis2", () => Int8Array.prototype.at.call({ length: 1, 0: 1 }, 0));
t("locale", () => new Uint8Array([1, 2]).toLocaleString());
t("subclass", () => { class X extends Uint8Array {} var x = X.from([1, 2]); return (x instanceof X) + ":" + x.join(); });
out.join(" | ");
