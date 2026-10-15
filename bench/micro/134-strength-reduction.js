// Multiplies, divides and modulos by constants that are powers of two, and constant folding.
// features: loop, call
// expect: 65533016704
var K = 4;
function reduce(x) {
  var a = x * 8, b = (x / 4) | 0, c = x % 16, d = x * (2 + 2), e = (x << 1) * K;
  return (a + b + c + d + e) & 0xffff;
}
function main() {
  var s = 0;
  for (var i = 0; i < 2000000; i++) s += reduce(i);
  return s;
}
main();
