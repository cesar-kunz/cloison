// The Set methods (`union` …, over set-likes), RegExp's
// `Symbol.match`-style methods, and String methods calling a pattern
// object's own.
// expect: union 1,2,3,4 | inter 2,3|5,1 | diff 1|2,3,4 | symdiff 1,4 | subset true,false | superset true,false | disjoint true,false | mapLike 1,2,3,7 | setLike 1 | badSize !TypeError | negSize !RangeError | noHas !TypeError | array !TypeError | negZero true | rxMatch baXcX | rxSearch 2,3,2 | custom abc/R | customSplit custom,a-b,3 | customMatch m:x42 | plainObj a_b | badRecv !TypeError

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
function t(name, f) { try { log(name, f()); } catch (e) { log(name, "!" + e.name); } }
var A = new Set([1, 2, 3]), B = new Set([2, 3, 4]);
var show = s => [...s].join(",");
t("union", () => show(A.union(B)));
t("inter", () => show(A.intersection(B)) + "|" + show(new Set([1,2,3,4,5]).intersection(new Set([5, 1]))));
t("diff", () => show(A.difference(B)) + "|" + show(new Set([1,2,3,4,5]).difference(new Set([5, 1]))));
t("symdiff", () => show(A.symmetricDifference(B)));
t("subset", () => [new Set([2]).isSubsetOf(A), A.isSubsetOf(B)].join());
t("superset", () => [A.isSupersetOf(new Set([1, 3])), A.isSupersetOf(B)].join());
t("disjoint", () => [A.isDisjointFrom(new Set([9])), A.isDisjointFrom(B)].join());
t("mapLike", () => show(A.union(new Map([[7, "x"]]))));
t("setLike", () => show(A.intersection({ size: 2, has: x => x == 1, keys: () => [1, 9][Symbol.iterator]() })));
t("badSize", () => A.union({ size: NaN, has() {}, keys() {} }));
t("negSize", () => A.union({ size: -1, has() {}, keys() {} }));
t("noHas", () => A.union({ size: 1, keys() {} }));
t("array", () => A.union([1]));
t("negZero", () => Object.is([...new Set().union(new Set([-0]))][0], 0));
t("rxMatch", () => /b/[Symbol.match]("abc")[0] + RegExp.prototype[Symbol.replace].call(/b/g, "abcb", "X"));
t("rxSearch", () => /c/[Symbol.search]("abc") + "," + /,/[Symbol.split]("a,b,c").length + "," + [.../\d/g[Symbol.matchAll]("a1b2")].length);
t("custom", () => "abc".replace({ [Symbol.replace](s, r) { return s + "/" + r; } }, "R"));
t("customSplit", () => "a-b".split({ [Symbol.split](s, lim) { return ["custom", s, lim]; } }, 3).join());
t("customMatch", () => "x".match({ [Symbol.match](s) { return "m:" + s; } }) + "x".search({ [Symbol.search]() { return 42; } }));
t("plainObj", () => "a[object Object]b".replace({}, "_"));
t("badRecv", () => RegExp.prototype[Symbol.match].call({}, "x"));
out.join(" | ");
