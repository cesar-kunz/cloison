// Counted int32 loop summing the counter.
// features: loop
// expect: 1783293664
function main() {
  var sum = 0;
  for (var i = 0; i < 1000000; i++) sum = (sum + i) | 0;
  return sum;
}
main();
