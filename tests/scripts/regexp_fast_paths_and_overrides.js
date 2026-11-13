// String methods' RegExp fast paths step aside where the spec's paths
// differ: a sticky separator splits as any, a RegExp whose `Symbol.<m>`
// is undefined is its string, redefined flags are what it has, and
// `replaceAll` reads `flags`.
// expect: [["a","b","c"],["a","1","b","2","c"],["","a",""]] a|c 1 aXcb b TypeError aXcb TypeError aXcX R

var r = [];
function t(f) { try { return String(f()); } catch (e) { return e.name; } }
r.push(JSON.stringify(["a,b,c".split(/,/y), "a1b22c".split(/(\d)+/y), "xax".split(/x/y)]));
var re = /b/; re[Symbol.split] = undefined; r.push("a/b/c".split(re).join("|"));
var re2 = /b/; re2[Symbol.search] = undefined; r.push("a/b/c".search(re2));
var g = /b/g; Object.defineProperty(g, "global", { value: false }); r.push("abcb".replace(g, "X"), String("abcb".match(g)));
var g2 = /b/g; Object.defineProperty(g2, "flags", { value: "" }); r.push(t(() => "abcb".replaceAll(g2, "X")));
var g3 = /b/g; Object.defineProperty(g3, "flags", { value: "" }); r.push("abcb".replace(g3, "X"));
r.push(t(() => "abcb".replaceAll(/b/, "X")), "abcb".replaceAll(/b/g, "X"));
r.push(t(() => "a".replaceAll({ [Symbol.match]: true, flags: "g", [Symbol.replace]() { return "R"; } }, "")));
r.join(" ");
