// Integer division by `(a / b) | 0` and `%` with variable and constant divisors.
// features: loop, call
// expect: 13500000
function digitsSum(n) {
  var s = 0;
  while (n > 0) { s += n % 10; n = (n / 10) | 0; }
  return s;
}
function main() {
  var t = 0;
  for (var i = 0; i < 500000; i++) t += digitsSum(i) + (i % ((i & 7) + 1));
  return t;
}
main();
