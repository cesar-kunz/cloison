// Strict code's store that can't happen throws a `TypeError` (to a
// read-only or frozen property, a non-extensible object, a primitive, an
// accessor without a setter); sloppy code's does nothing
// (`Vm::store_failed`), compiled code's as the interpreter's.
// expect: frozen:TypeError,readonly:TypeError,nonext:TypeError,getter:TypeError,strlen:TypeError,num:TypeError,inherited:TypeError,frozenarr:TypeError,frozenpush:TypeError,elemro:TypeError,keyed:TypeError,sloppy:ok,ok:ok,5,1,5,1,2

var r = [];
function t(name, f) { try { f(); r.push(name + ":ok"); } catch (e) { r.push(name + ":" + e.name); } }
var fr = Object.freeze({ a: 1 }), ro = {}; Object.defineProperty(ro, "x", { value: 1 });
var ne = Object.preventExtensions({}), go = { get g() { return 1; } }, fa = Object.freeze([1, 2]);
var inh = Object.create(ro), one = [1, 2]; Object.defineProperty(one, 0, { writable: false });
t("frozen", function () { "use strict"; fr.a = 2; });
t("readonly", function () { "use strict"; ro.x = 2; });
t("nonext", function () { "use strict"; ne.y = 1; });
t("getter", function () { "use strict"; go.g = 2; });
t("strlen", function () { "use strict"; "abc".length = 1; });
t("num", function () { "use strict"; (1).x = 1; });
t("inherited", function () { "use strict"; inh.x = 5; });
t("frozenarr", function () { "use strict"; fa[0] = 9; });
t("frozenpush", function () { "use strict"; fa[5] = 9; });
t("elemro", function () { "use strict"; one[0] = 9; });
t("keyed", function () { "use strict"; var k = "a"; fr[k] = 3; });
t("sloppy", function () { fr.a = 2; ro.x = 2; ne.y = 1; go.g = 2; (1).x = 1; fa[0] = 9; one[0] = 9; });
t("ok", function () { "use strict"; var o = { a: 1 }; o.a = 2; o.b = 3; o["c"] = 4; one[1] = 5; });
var n = 0; function hot(o) { "use strict"; o.a = 1; } for (var i = 0; i < 5000; i++) { try { hot(i % 1000 == 999 ? fr : {}); } catch (e) { n++; } }
r.push(n, one.join(), fa.join());
r.join();
