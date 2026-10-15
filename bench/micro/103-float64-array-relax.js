// Jacobi-style relaxation over a Float64Array grid (Navier-Stokes' lin_solve).
// features: loop, double, typed-array
// expect: 5627696
function relaxGrid(x, x0, w, h, a, c) {
  for (var j = 1; j < h - 1; j++) {
    var row = j * w;
    for (var i = 1; i < w - 1; i++) {
      var k = row + i;
      x[k] = (x0[k] + a * (x[k - 1] + x[k + 1] + x[k - w] + x[k + w])) / c;
    }
  }
}
function main() {
  var w = 64, h = 64, x = new Float64Array(w * h), x0 = new Float64Array(w * h);
  for (var k = 0; k < w * h; k++) x0[k] = (k % 13) * 0.25;
  for (var r = 0; r < 300; r++) relaxGrid(x, x0, w, h, 0.5, 3);
  var s = 0;
  for (var k = 0; k < w * h; k++) s += x[k];
  return Math.round(s * 1000);
}
main();
