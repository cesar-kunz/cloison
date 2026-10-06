// `x || 0` defaulting an argument that is often undefined.
// features: loop, call
// expect: 62500000000
function pick(name, initial) { return initial || 0; }
function main() {
  var sum = 0;
  for (var i = 0; i < 500000; i++) {
    sum += (i & 1) ? pick("v", i) : pick("v");
  }
  return sum;
}
main();
