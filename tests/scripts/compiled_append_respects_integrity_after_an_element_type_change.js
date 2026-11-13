// A sealed or non-extensible array whose elements change type (an int32
// array given a double) still takes no element through compiled code:
// its shape's elements kept no type, so no fast path appends.
// expect: 1000 3 1000 3 1.5,2,3

"use strict";
function f(a, v) { a[a.length] = v; }
for (var i = 0; i < 100000; i++) f([1, 2], i);
function errs(a) { var n = 0; for (var i = 0; i < 1000; i++) { try { f(a, 7); } catch (e) { n++; } } return n; }
var s = [1, 2, 3], p = [1, 2, 3];
Object.seal(s); Object.preventExtensions(p);
s[0] = 1.5; p[0] = 1.5;
[errs(s), s.length, errs(p), p.length, s.join()].join(' ');
