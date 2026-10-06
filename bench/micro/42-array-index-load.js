// Indexed loads from a packed int array in a counted loop.
// features: loop, array
// expect: 1498500000
function main() {
  var a = [];
  for (var i = 0; i < 1000; i++) a.push(i * 3);
  var sum = 0;
  for (var r = 0; r < 1000; r++) for (var i = 0; i < a.length; i++) sum = (sum + a[i]) | 0;
  return sum;
}
main();
