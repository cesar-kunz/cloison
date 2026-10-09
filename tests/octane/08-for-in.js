// for-in over own and inherited properties, the Object.extend pattern.
// for: raytrace (Object.extend), earley-boyer, pdfjs, typescript
// expect: own;inherited;6;2
Object.extend = function (dst, src) { for (var p in src) dst[p] = src[p]; return dst; };
function B() { this.own = 1; }
B.prototype.inherited = 2;
var keys = "";
for (var k in new B()) keys += k + ";";
var o = Object.extend({ a: 1 }, { b: 2, c: 3 });
var n = 0;
for (var k2 in o) n += o[k2];
keys + n + ";" + Object.extend(Object.extend({}, { x: 1 }), { y: 2 }).y;
