// expect: f64:RangeError,i8:RangeError,like:RangeError,ok:16,loop:RangeError

var r = [];
function t(name, f) { try { r.push(name + ":" + f()); } catch (e) { r.push(name + ":" + e.name); } }
t("f64", function () { return new Float64Array(Math.pow(2, 32)).length; });
t("i8", function () { return new Int8Array(5e9).length; });
t("like", function () { return new Uint8Array({ length: 5e9 }).length; });
t("ok", function () { return new Uint8Array(16).length; });
t("loop", function () { var s = "x"; for (var i = 0; i < 40; i++) s += s; return s.length; });
r.join();
