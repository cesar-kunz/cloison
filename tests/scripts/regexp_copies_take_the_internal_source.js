// `RegExp` and `compile` copy a RegExp's internal source, not what a
// redefined `source` getter says (nor do the species copies of
// `matchAll` and `split`); `RegExp.prototype[Symbol.matchAll]` takes a
// non-global RegExp (the "g" check is `String.prototype.matchAll`'s,
// which converts its receiver before its pattern).
// expect: 1 2 TypeError [["a"]] 1 1 recv,pat Error true true true true true 2 c|c true

var r = [];
function t(f) { try { return String(f()); } catch (e) { return e.name; } }
var ma = RegExp.prototype[Symbol.matchAll];
r.push([...ma.call(/a/, "aa")].length, [.../a/g[Symbol.matchAll]("aa")].length, t(() => "aa".matchAll(/a/)));
var y = /a/y; y.lastIndex = 1;
r.push(JSON.stringify([...ma.call(y, "aab")]), y.lastIndex);
var log = [];
var recv = { toString() { log.push("recv"); return "aXa"; } }, pat = { toString() { log.push("pat"); return "X"; } };
r.push([...String.prototype.matchAll.call(recv, pat)].length, log.join());
r.push(t(() => String.prototype.matchAll.call({ toString() { throw new Error("receiver"); } }, "[")));
Object.defineProperty(RegExp.prototype, "source", { get() { return "b"; } });
r.push(new RegExp(/a/).test("a"), new RegExp(/a/, "i").test("A"), RegExp(/a/, "g").test("a"), /x/.compile(/a/).test("a"));
class S extends RegExp {}
r.push(new S(/a/).test("a"), [..."aa".matchAll(new S("a", "g"))].length, "cac".split(new S("a")).join("|"));
r.push(new RegExp({ [Symbol.match]: true, source: "c", flags: "" }).test("c"));
r.join(" ");
