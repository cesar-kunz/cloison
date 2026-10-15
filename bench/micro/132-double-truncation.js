// Doubles truncated to ints with `| 0`, `>> 0` and Math.floor, used as array indices.
// features: loop, double, array, call, math
// expect: 27888000
function bucketOf(x, buckets) {
  var a = (x * buckets) | 0, b = (x * 7.5) >> 0, c = Math.floor(x * 3.25);
  return (a + b + c) % buckets;
}
function main() {
  var hist = [];
  for (var i = 0; i < 64; i++) hist.push(0);
  for (var i = 0; i < 1000000; i++) hist[bucketOf((i % 1000) / 1000, 64)]++;
  var s = 0;
  for (var i = 0; i < 64; i++) s += hist[i] * i;
  return s;
}
main();
