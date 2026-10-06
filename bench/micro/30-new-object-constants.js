// An enum built with `new Object()` plus property stores, read in a loop.
// features: loop, object, new, global
// expect: 3333333
var Dir = new Object();
Dir.NONE = 0;
Dir.FORWARD = 1;
Dir.BACKWARD = -1;
function main() {
  var sum = 0;
  for (var i = 0; i < 1000000; i++) {
    var d = (i % 3) - 1;
    if (d == Dir.FORWARD) sum += 2;
    else if (d == Dir.BACKWARD) sum += 3;
    else if (d == Dir.NONE) sum += 5;
  }
  return sum;
}
main();
