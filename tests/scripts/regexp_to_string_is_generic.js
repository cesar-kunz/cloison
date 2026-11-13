// `RegExp.prototype.toString` is generic (#132): any object's `source`
// and `flags`, each through ToString, also from compiled `re + ""`.
// expect: /a/g /x/zz TypeError 7 /a\/b/gimsuy /(?:)/ /q/y /w/F /x/zz

function t(f) { try { return String(f()); } catch (e) { return typeof e == "number" ? e : e.name; } }
var re = /x/g; Object.defineProperty(re, "flags", { value: "zz" });
var o = { source: "s", get flags() { return { toString() { throw 7; } }; }, toString: RegExp.prototype.toString };
function f(r) { return r + ""; }
var s; for (var i = 0; i < 20000; i++) s = f(/q/y);
var r2 = /w/m; Object.defineProperty(r2, "flags", { get() { return "F"; } });
[t(() => RegExp.prototype.toString.call({ source: "a", flags: "g" })), String(re),
 t(() => RegExp.prototype.toString.call(1)), t(() => o + ""), String(/a\/b/gimsuy),
 String(RegExp.prototype), s, f(r2), f(re)].join(" ");
