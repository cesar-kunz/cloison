// Constructor code several classes share (Prototype's `Class.create`)
// tracks each prototype's capacity: a class with properties added after
// its constructor, one inside it, and one whose `initialize` adds more
// later. The expected value is Node's.
// expect: 4008068000,s0,9,18,9.5,s0,3,2,7,pabcde

var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var P = Class.create(); P.prototype = { initialize: function (x, y) { this.x = x; this.y = y; } };
var Big = Class.create(); Big.prototype = { initialize: function () { this.p = new P(1, 2); } };
function hit(n) { var b = new Big(); b.a = n; b.b = n * 2; b.c = n + 0.5; b.d = "s" + (n % 3); b.e = b.p.x + b.p.y; return b; }
var s = 0, t = "";
for (var i = 0; i < 40000; i++) { var b = hit(i); s += b.a + b.b + b.c + b.e + b.p.x * i; t = b.d; }
P.prototype.initialize = function (x, y) { this.x = x; this.y = y; this.z = x * y; this.w = 7; };
for (var i = 0; i < 2000; i++) { var q = new P(i, 3); s += q.z + q.w + q.x; var b = hit(i); s += b.p.z + b.p.w; }
var last = hit(9);
[s, t, last.a, last.b, last.c, last.d, last.e, last.p.z, last.p.w, Object.keys(last).join("")].join();
