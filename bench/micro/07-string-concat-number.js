// Building names by concatenating a string and a number.
// features: loop, string
// expect: 1288890
function main() {
  var total = 0;
  for (var i = 0; i < 200000; i++) {
    var name = "v" + i;
    total += name.length;
  }
  return total;
}
main();
