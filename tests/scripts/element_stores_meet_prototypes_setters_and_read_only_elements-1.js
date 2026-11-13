// A store adding an element calls a setter, or fails on a read-only
// element, on the prototype chain (`push` too), once one exists there;
// code compiled before then stops storing elements directly. Literals
// and builtins that define elements don't call setters.
// expect: arraySetter 3,proto,1,false | pushSetter 4,2 | hotSetter 3000 | readonlyProto ro,false | readonlyArr ro,0 | strictRO !TypeError | hotRO 3000 | objProtoSetter 7,false | hotObjProto 3000 | frozenProtoArr 1 | normal 100

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
function t(name, f) { try { log(name, f()); } catch (e) { log(name, "!" + e.name); } }
var calls = 0;
Object.defineProperty(Array.prototype, "3", { set(v) { calls++; }, get() { return "proto"; }, configurable: true });
t("arraySetter", () => { var a = [0, 1, 2]; a[3] = 9; return [a.length, a[3], calls, a.hasOwnProperty(3)].join(); });
t("pushSetter", () => { var a = [0, 1, 2]; a.push(9); return [a.length, calls].join(); });
t("hotSetter", () => { var n = 0; for (var i = 0; i < 3000; i++) { var a = [0, 1, 2]; a[3] = i; if (!a.hasOwnProperty(3)) n++; } return n; });
delete Array.prototype[3];
Object.defineProperty(Object.prototype, "1", { value: "ro", writable: false, configurable: true });
t("readonlyProto", () => { var o = {}; o[1] = 5; return o[1] + "," + o.hasOwnProperty(1); });
t("readonlyArr", () => { var a = []; a[1] = 5; return a[1] + "," + a.length; });
t("strictRO", () => { "use strict"; var o = {}; o[1] = 5; return o[1]; });
t("hotRO", () => { var n = 0; for (var i = 0; i < 3000; i++) { var o = []; o[1] = i; if (o[1] === "ro") n++; } return n; });
delete Object.prototype[1];
var proto = { set 0(v) { this.seen = v; } };
t("objProtoSetter", () => { var o = Object.create(proto); o[0] = 7; return [o.seen, o.hasOwnProperty(0)].join(); });
t("hotObjProto", () => { var n = 0; for (var i = 0; i < 3000; i++) { var o = Object.create(proto); o[0] = i; if (o.seen === i) n++; } return n; });
var frozenProto = Object.freeze([1, 2]); 
t("frozenProtoArr", () => { var a = Object.create(frozenProto); a[0] = 5; return a[0]; });
t("normal", () => { var a = []; for (var i = 0; i < 100; i++) a[i] = i; return a.length; });
out.join(" | ");
