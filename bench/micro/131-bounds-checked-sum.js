// The canonical `for (i = 0; i < a.length; i++) s += a[i]` (bounds checks the loop condition proves).
// features: loop, array, call
// expect: 623580000
function sumArray(a) {
  var s = 0;
  for (var i = 0; i < a.length; i++) s += a[i];
  return s;
}
function main() {
  var a = [];
  for (var i = 0; i < 1000; i++) a.push(i & 255);
  var t = 0;
  for (var r = 0; r < 5000; r++) t += sumArray(a);
  return t;
}
main();
