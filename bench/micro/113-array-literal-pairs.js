// Small array literals as tuples: returned as [quotient, remainder], then read.
// features: loop, array, call, literal
// expect: 71425071432
function divmod(a, b) { return [(a / b) | 0, a % b]; }
function main() {
  var s = 0;
  for (var i = 1; i < 1000000; i++) {
    var qr = divmod(i, 7);
    s += qr[0] - qr[1];
  }
  return s;
}
main();
