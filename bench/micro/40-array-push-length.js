// Growing an empty `new Array()` with push and reading length.
// features: loop, array, new
// expect: 300000
function main() {
  var total = 0;
  for (var r = 0; r < 1000; r++) {
    var a = new Array();
    for (var i = 0; i < 300; i++) a.push(i);
    total += a.length;
  }
  return total;
}
main();
