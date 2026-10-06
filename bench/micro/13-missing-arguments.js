// Calling with fewer arguments than parameters; the rest are undefined.
// features: loop, call, object, new
// expect: 30000400000
function Box(name, initial, extra) {
  this.value = initial || 0;
  this.extra = extra === undefined ? 1 : 2;
}
function main() {
  var sum = 0;
  for (var i = 0; i < 300000; i++) {
    var b = (i % 3 == 0) ? new Box("b") : (i % 3 == 1) ? new Box("b", i) : new Box("b", i, i);
    sum += b.value + b.extra;
  }
  return sum;
}
main();
