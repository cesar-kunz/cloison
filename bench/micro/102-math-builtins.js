// Math.floor, sqrt, abs, min and max on doubles in a hot loop.
// features: loop, double, call, math
// expect: 15209179
function clampDist(x, y) {
  var d = Math.sqrt(x * x + y * y);
  return Math.min(Math.max(Math.abs(d - 50), 1), 40);
}
function main() {
  var s = 0;
  for (var i = 0; i < 1000000; i++) s += Math.floor(clampDist(i % 97 - 48.5, i % 89 - 44.5));
  return s;
}
main();
