// Nested ternaries over int comparisons picking a direction.
// features: loop
// expect: 134393572143
function main() {
  var forward = 0, backward = 0, none = 0;
  for (var i = 0; i < 300000; i++) {
    var a = i % 7, b = i % 5, s = i % 3;
    var d = a < b ? (s < a ? 1 : 0) : (s < b ? 1 : -1);
    if (d == 1) forward++;
    else if (d == -1) backward++;
    else none++;
  }
  return forward * 1000000 + backward * 1000 + none % 1000;
}
main();
