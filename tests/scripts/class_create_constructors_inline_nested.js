// Prototype's `Class.create` constructors (`this.initialize.apply(this,
// arguments)`) inlined at any depth, their `initialize` within the limit:
// a `new` in an inlined `initialize`, then `initialize` replaced. The
// expected value is Node's.
// expect: 836915329,40,2,1,true

var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var Color = Class.create();
Color.prototype = { initialize: function (r, g, b) { if (!r) r = 0.0; if (!g) g = 0.0; if (!b) b = 0.0; this.red = r; this.green = g; this.blue = b; } };
var Info = Class.create();
Info.prototype = { initialize: function () { this.color = new Color(0, 0, 0); this.hit = false; } };
var Shape = Class.create();
Shape.prototype = { initialize: function (k) { this.k = k; }, intersect: function (n) { var info = new Info(); if (n % 3) { info.hit = true; info.color = new Color(n, this.k, arguments.length); } return info; } };
var sh = new Shape(2), s = 0;
for (var i = 0; i < 50000; i++) { var r = sh.intersect(i); s += r.color.red + r.color.green * 2 + r.color.blue * 3 + (r.hit ? 1 : 0); }
Color.prototype.initialize = function (r, g, b) { this.red = r * 10; this.green = g; this.blue = b; };
for (var i = 0; i < 1000; i++) { var r = sh.intersect(i); s += r.color.red + r.color.green * 2 + r.color.blue * 3 + (r.hit ? 1 : 0); }
var x = sh.intersect(4);
[s, x.color.red, x.color.green, x.color.blue, x.hit].join();
