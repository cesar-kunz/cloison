// Dot products and axpy over plain arrays holding doubles.
// features: loop, double, array, call
// expect: 10446100
function dotArrays(a, b) {
  var s = 0;
  for (var i = 0; i < a.length; i++) s += a[i] * b[i];
  return s;
}
function axpy(y, a, x) {
  for (var i = 0; i < y.length; i++) y[i] = y[i] * 0.5 + a * x[i];
}
function main() {
  var x = [], y = [];
  for (var i = 0; i < 256; i++) { x.push(i * 0.5); y.push(1.5 - i * 0.25); }
  var s = 0;
  for (var r = 0; r < 4000; r++) {
    axpy(y, 0.001, x);
    s += dotArrays(x, y);
  }
  return Math.round(s);
}
main();
