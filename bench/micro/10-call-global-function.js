// Calling a small top-level function from a loop.
// features: loop, call
// expect: 230769
function less(a, b) { return a < b; }
function main() {
  var n = 0;
  for (var i = 0; i < 1000000; i++) if (less(i % 13, i % 7)) n++;
  return n;
}
main();
