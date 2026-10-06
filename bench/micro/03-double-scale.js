// Linear map forward (x * scale + offset) and backward ((y - offset) / scale).
// features: loop, double
// expect: 89999790000.28699
function main() {
  var scale = 10, offset = 1000, acc = 0;
  for (var i = 0; i < 300000; i++) {
    var y = i * scale + offset;
    var x = (y - offset) / scale;
    var z = (y + 3 - offset) / scale;
    acc += x + z;
  }
  return acc;
}
main();
