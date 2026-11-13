// Mapped `arguments` read their parameters (also through a cached keyed
// load, and when a closure assigns one); `for … in` skips a key deleted
// before its turn; an indexed getter on a prototype gets the receiver.
// expect: 1,18000,42,9,abd,ac,0x2,1x2,y,true,8,7,object

var r = [];
function f1(a) { var t = 0; for (var i = 0; i < 2; i++) { a = i; t += arguments[0]; } return t; }
r.push(f1(0));
function k(a) { var s = 0; for (var i = 0; i < 3; i++) { a = i + 1; s += arguments[0]; } return s; }
var ks = 0; for (var j = 0; j < 3000; j++) ks += k(0); r.push(ks);
function f2(x) { (function () { x = 42; })(); return arguments[0]; }
r.push(f2(0));
function f3(x) { arguments[0] = 9; return (function () { return x; })(); }
r.push(f3(1));
var d = { a: 1, b: 2, c: 3, d: 4 }, s = []; for (var key in d) { s.push(key); if (key == "a") delete d.c; }
r.push(s.join(""));
var e = { a: 1, b: 2, c: 3 }, s2 = []; function del(o) { delete o.b; } for (var key in e) { s2.push(key); del(e); }
r.push(s2.join(""));
var str = [], z; for (z in "ab") str.push(z + "x" + "ab".length); r.push(str.join());
var P = { x: 1 }; var C = Object.create(P); C.y = 2; var s3 = []; for (var key in C) { s3.push(key); if (key == "y") delete P.x; } r.push(s3.join(""));
var h = [1, , 3]; Object.defineProperty(Array.prototype, "1", { get: function () { return this === h; }, configurable: true });
r.push(h[1]); delete Array.prototype[1];
var o = { q: 7 }; Object.defineProperty(o, "2", { get: function () { return this.q; } }); var ch = Object.create(o); ch.q = 8; r.push(ch[2], o[2]);
Object.defineProperty(String.prototype, "5", { get: function () { return typeof this; }, configurable: true }); r.push("abc"[5]);
r.join();
