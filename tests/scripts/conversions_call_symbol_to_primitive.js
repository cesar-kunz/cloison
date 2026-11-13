// Conversions call an object's `Symbol.toPrimitive` method, with the
// hint, before `valueOf` and `toString`.
// expect: plus=42 add=sdefault1 tmpl=sstring str=sstring lt=true eq=true arith=84 bad=!TypeError obj=!TypeError thr=!RangeError nul=7 cls=default hot=126000 date=string hints=number,default,string,string,number,default,number

var r = [];
function t(n, f) { var v; try { v = String(f()); } catch (e) { v = "!" + e.name; } r.push(n + "=" + v); }
var hints = [];
var o = { [Symbol.toPrimitive](h) { hints.push(h); return h == "number" ? 42 : "s" + h; }, valueOf() { return 1; } };
t("plus", () => +o);
t("add", () => o + 1);
t("tmpl", () => `${o}`);
t("str", () => String(o));
t("lt", () => o < 50);
t("eq", () => o == "sdefault");
t("arith", () => o * 2);
t("bad", () => +{ [Symbol.toPrimitive]: 1 });
t("obj", () => +{ [Symbol.toPrimitive]() { return {}; } });
t("thr", () => +{ [Symbol.toPrimitive]() { throw new RangeError(); } });
t("nul", () => +{ [Symbol.toPrimitive]: null, valueOf() { return 7; } });
class D { [Symbol.toPrimitive](h) { return h; } }
t("cls", () => new D() + "");
var hot = 0; for (var i = 0; i < 3000; i++) hot += +o; t("hot", () => hot);
t("date", () => typeof (new Date(0) + 1));
t("hints", () => hints.slice(0, 7).join());
r.join(" ");
