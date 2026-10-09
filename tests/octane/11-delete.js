// `delete` of properties: then missing, re-added, of indices, of
// assigned globals (zlib's tearDown).
// for: zlib (133), earley-boyer, pdfjs, box2d
// expect: true,acacb,9,3,,false,true,undefined,true
var o = { a: 1, b: 2, c: 3 };
var r1 = delete o.b;
var keys = "";
for (var k in o) keys += k;
o.b = 9;
for (var k2 in o) keys += k2;
var arr = [1, 2, 3];
delete arr[1];
g1 = 5;
var r2 = delete g1;
[r1, keys, o.b, arr.length, arr[1], 1 in arr, r2, typeof g1, delete o.zz].join(",");
