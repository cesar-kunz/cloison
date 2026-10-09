// `arguments`: length, indexing, passed on, in a nested function's own.
// for: raytrace, earley-boyer, box2d, pdfjs, mandreel, typescript
// expect: 0,2,6,9/9/undefined,7,2013000
function count() { return arguments.length; }
function sum() {
  var s = 0;
  for (var i = 0; i < arguments.length; i++) s += arguments[i];
  return s;
}
function first(a, b) { return arguments[0] + "/" + a + "/" + arguments[5]; }
function outer(x) {
  var args = arguments;
  return function () { return args[0] + arguments.length; };
}
var t = 0;
for (var k = 0; k < 2000; k++) t += sum(k, 1, 2) + count(1, 2, 3, 4);
[count(), count(1, undefined), sum(1, 2, 3), first(9), outer(5)(1, 2), t].join(",");
