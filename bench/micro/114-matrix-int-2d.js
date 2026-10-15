// Integer matrix multiply over arrays of arrays.
// features: loop, array, call
// expect: 32800
function makeMatrix(n, seed) {
  var m = [];
  for (var i = 0; i < n; i++) {
    var row = [];
    for (var j = 0; j < n; j++) row.push((i * seed + j) % 10);
    m.push(row);
  }
  return m;
}
function matMul(a, b, c, n) {
  for (var i = 0; i < n; i++) {
    var ai = a[i], ci = c[i];
    for (var j = 0; j < n; j++) {
      var s = 0;
      for (var k = 0; k < n; k++) s += ai[k] * b[k][j];
      ci[j] = s;
    }
  }
}
function main() {
  var n = 40, a = makeMatrix(n, 3), b = makeMatrix(n, 7), c = makeMatrix(n, 1), t = 0;
  for (var r = 0; r < 40; r++) {
    matMul(a, b, c, n);
    t += c[r % n][(r * 3) % n];
  }
  return t;
}
main();
