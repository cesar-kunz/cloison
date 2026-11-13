// `at`, `findLast`, the copying array methods, `replaceAll`,
// `matchAll`, `Object.fromEntries`/`hasOwn`/`groupBy`, `Map.groupBy`
// and errors' `cause`.
// expect: 3 undefined c 1 | 3 -1 1 | 1,2,3 3,1,2 2,1,3 3,1,9 3,a,b,2 | 3 1,5,10 | RangeError | a-b-c aaaaaa _a_b_c_ a1b3c | a#b#c# | TypeError | 1@1,22@3,333@6 | 5 | TypeError | {"a":1,"b":2} {"m":3} | true false | null {"odd":[1,3,5],"even":[2,4]} | 2,3 1 2 | why 0 false 0

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
log([1,2,3].at(-1), [1,2,3].at(5), "abc".at(-1), [1,2,3].at(0.5));
log([1,2,3,4].findLast(x => x % 2), [1,2,3,4].findLastIndex(x => x > 9), [1,2,3,4].findLastIndex(x => x < 3));
var orig = [3,1,2]; log(orig.toSorted().join(), orig.join(), orig.toReversed().join(), orig.with(-1, 9).join(), orig.toSpliced(1, 1, "a", "b").join());
log([1,,3].toSorted().length, [5, 1, 10].toSorted((a, b) => a - b).join());
try { [1].with(5, 0) } catch (e) { log(e.name) }
log("aXbXc".replaceAll("X", "-"), "aaa".replaceAll("a", "$&$&"), "abc".replaceAll("", "_"), "a.b.c".replaceAll(".", (m, i) => i));
log("a1b2c3".replaceAll(/\d/g, "#"));
try { "a".replaceAll(/a/, "b") } catch (e) { log(e.name) }
log([..."a1b22c333".matchAll(/\d+/g)].map(m => m[0] + "@" + m.index).join());
log([..."x.y.z".matchAll(".")].length);
try { "a".matchAll(/a/) } catch (e) { log(e.name) }
log(JSON.stringify(Object.fromEntries([["a", 1], ["b", 2]])), JSON.stringify(Object.fromEntries(new Map([["m", 3]]))));
log(Object.hasOwn({a: 1}, "a"), Object.hasOwn({a: 1}, "toString"));
var g = Object.groupBy([1, 2, 3, 4, 5], x => x % 2 ? "odd" : "even"); log(Object.getPrototypeOf(g), JSON.stringify(g));
var mg = Map.groupBy([1, 2, 3], x => x > 1); log(mg.get(true).join(), mg.get(false).join(), mg.size);
var e = new Error("x", {cause: "why"}); log(e.cause, Object.keys(e).length, "cause" in new Error("y"), new TypeError("t", {cause: 0}).cause);
out.join(" | ");
