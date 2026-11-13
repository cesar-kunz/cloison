// A strict function called without a receiver gets `undefined` as `this`
// from compiled code too: inlined (also among several callees), called
// directly, and in a self tail call.
// expect: 20000,20000,0,5000,false/object/true/undefined

function s1() { "use strict"; return this === undefined; }
function big() {
  "use strict";
  var x = 0;
  for (var j = 0; j < 3; j++) { x += j * 2; x ^= j; x += j % 3; x -= 1; x *= 1; x += 7; x -= 7; x |= 0; }
  return (this === undefined ? 1 : 2) + x - x;
}
function sloppy() { return this === undefined; }
function tr(n, acc) { "use strict"; if (n == 0) return acc; return tr(n - 1, acc + (this === undefined ? 1 : 0)); }
var fs = [s1, sloppy, function () { "use strict"; return typeof this; }, function () { return typeof this; }];
var a = 0, b = 0, c = 0, q = {};
for (var i = 0; i < 20000; i++) { if (s1()) a++; b += big(); if (sloppy()) c++; }
for (var i = 0; i < 20000; i++) { var g = fs[i & 3]; q[g()] = 1; }
[a, b, c, tr(5000, 0), Object.keys(q).sort().join("/")].join();
