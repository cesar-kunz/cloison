// Park-Miller generator in doubles: products past int32, then `%` by a constant.
// features: loop, double, call
// expect: 999790
function nextSeed(s) { return (s * 16807) % 2147483647; }
function main() {
  var s = 42, n = 0;
  for (var i = 0; i < 2000000; i++) {
    s = nextSeed(s);
    if (s < 1073741824) n++;
  }
  return n + s % 1000;
}
main();
