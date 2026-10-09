// Function.prototype.apply: with an array, with `arguments` (raytrace's
// Class.create: `this.initialize.apply(this, arguments)`, 80M calls), with
// null, without an array.
// for: raytrace, box2d, pdfjs, code-load
// expect: 4504500,13,7,NaN,9,hi
var Class = { create: function () { return function () { this.initialize.apply(this, arguments); }; } };
var P = Class.create();
P.prototype = { initialize: function (x, y) { this.x = x; this.y = y; } };
function add(a, b) { return (this.k || 0) + a + b; }
var s = 0;
for (var i = 0; i < 3000; i++) { var p = new P(i, 2); s += p.x + p.y; }
[s, add.apply({ k: 10 }, [1, 2]), add.apply(null, [3, 4]), add.apply(undefined), Math.max.apply(Math, [3, 9, 2]),
 String.fromCharCode.apply(null, [104, 105])].join(",");
