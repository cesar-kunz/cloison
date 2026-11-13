// `isWellFormed`, `toWellFormed`, `Error.isError`, `Math.sumPrecise`,
// `RegExp.escape` and the `d` flag's `indices`.
// expect: false,true,a%uFFFDb%uFFFD true,false,false,true 1e+308,0.6,9007199254740992,true,NaN,Infinity TypeError %5Cx31a%5C.b%5C*c%5Cx20d%5Cx2c%5Cu2028%5Cud800 TypeError [[[2,4],null,[3,4]],null,"dg"]

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => ["ab\uD800".isWellFormed(), "a😀".isWellFormed(), escape("a\uDC00b\uD800".toWellFormed())].join());
class E extends TypeError {}
t(() => [Error.isError(new E), Error.isError(Object.create(Error.prototype)), Error.isError(new Proxy(new Error, {})),
  (function () { try { null.x; } catch (e) { return Error.isError(e); } })()].join());
t(() => [Math.sumPrecise([1e308, 1e308, -1e308]), Math.sumPrecise([0.1, 0.2, 0.3]), Math.sumPrecise([2 ** 53, 1]),
  Object.is(Math.sumPrecise([]), -0), Math.sumPrecise([Infinity, -Infinity]), Math.sumPrecise([1e308, 1e308])].join());
t(() => Math.sumPrecise([1, "2"]));
t(() => escape(RegExp.escape("1a.b*c d, \uD800")));
t(() => RegExp.escape(1));
t(() => { var m = /a(?<x>b)?(c)/d.exec("zzac"); return JSON.stringify([m.indices, m.indices.groups.x, /x/dg.flags]); });
r.join(" ");
