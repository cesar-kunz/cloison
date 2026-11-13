// What builds a string from pieces throws a RangeError past the longest
// string (2^30 units), as `+` and `repeat` do (#65): checked before a
// long piece is read, so a cons string of 2^30 units (2 MiB, its pieces
// shared) is never flattened.
// expect: plus:RangeError,join:RangeError,toLocaleString:RangeError,template:RangeError,concat:RangeError,raw:RangeError,stringify:RangeError,quoted:RangeError,iterator:RangeError,longest:1073741824

var s = "x".repeat(2 ** 20);
for (var i = 0; i < 10; i++) s = s + s;
var out = [];
function t(name, f) { try { out.push(name + ":" + f().length); } catch (e) { out.push(name + ":" + e.name); } }
t("plus", () => "x" + s);
t("join", () => ["x", s].join(""));
t("toLocaleString", () => ["x", { toLocaleString: () => s }].toLocaleString());
t("template", () => `x${s}`);
t("concat", () => "x".concat(s));
t("raw", () => String.raw({ raw: ["x", ""] }, s));
t("stringify", () => JSON.stringify(["x", s]));
t("quoted", () => JSON.stringify(s));
t("iterator", () => ["x", s].values().join(""));
t("longest", () => s);
out.join();
