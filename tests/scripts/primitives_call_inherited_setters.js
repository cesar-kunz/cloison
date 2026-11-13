// Stores to numbers, booleans and strings call their prototypes' setters,
// with the primitive for `this`.
// expect: number,5,string,6,number,7,object,8,3000

var r = [];
Object.defineProperty(Number.prototype, "sx", { set: function (v) { "use strict"; r.push(typeof this, v); }, configurable: true });
Object.defineProperty(String.prototype, "lx", { set: function (v) { "use strict"; r.push(typeof this, v); }, configurable: true });
Object.defineProperty(Boolean.prototype, "bx", { set: function (v) { r.push(typeof this, v); }, configurable: true });
(1).sx = 5; "s".lx = 6; var k = "sx"; (2)[k] = 7; true.bx = 8; "s".length = 1;
var n = 0; Object.defineProperty(Number.prototype, "hx", { set: function (v) { n += v; } });
function hot(p) { p.hx = 1; } for (var i = 0; i < 3000; i++) hot(i);
r.push(n);
r.join();
