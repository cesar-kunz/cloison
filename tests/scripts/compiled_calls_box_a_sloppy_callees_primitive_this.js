// A sloppy function compiled code calls, inlined or directly, or as an
// accessor, sees a string receiver as a `String` object, as the
// interpreter's calls give it; and a strict caller's `f.call(this)`
// passes its `this` as the interpreter converts it.
// expect: object object true object object 42 12502500

var r = [];
String.prototype.me = function () { return typeof this; };
function c1(n, s) { var x; for (var i = 0; i < n; i++) x = s.me(); return x; }
r.push(c1(5000, "abc"));
String.prototype.id = function () { return this; };
function c2(n, s) { var x; for (var i = 0; i < n; i++) x = s.id(); return x; }
var o = c2(5000, "abc"); r.push(typeof o, o instanceof String);
var body = "var a = typeof this;"; for (var j = 0; j < 200; j++) body += "a = a + '';"; body += "return a;";
String.prototype.huge = new Function(body);
function c3(n, s) { var x; for (var i = 0; i < n; i++) x = s.huge(); return x; }
r.push(c3(5000, "abc"));
Object.defineProperty(String.prototype, "gt", { get() { return typeof this; } });
function c4(n, s) { var x; for (var i = 0; i < n; i++) x = s.gt; return x; }
r.push(c4(5000, "abc"));
globalThis.xx = 42;
function sl() { return this.xx; }
function sc(n) { "use strict"; var x; for (var i = 0; i < n; i++) x = sl.call(this); return x; }
r.push(sc(5000));
function A() { this.v = 1; } A.prototype.m = function (k) { this.v = k; return this.v; };
function c5(n) { var a = new A(), s = 0; for (var i = 0; i < n; i++) s += a.m(i) + A.prototype.m.call(a, 1); return s; }
r.push(c5(5000));
r.join(" ");
