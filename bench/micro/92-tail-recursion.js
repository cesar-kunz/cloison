// A tail-recursive sum, 5000 deep, run 100 times from a loop.
// cloison: self tail calls compiled as loops, called directly from the loop
// features: loop, call, recursion
// expect: 1250250000
function sum(n, acc) { if (n < 1) return acc; return sum(n - 1, acc + n); }
function main() {
  var t = 0;
  for (var i = 0; i < 100; i++) t = t + sum(5000, 0);
  return t;
}
main();
