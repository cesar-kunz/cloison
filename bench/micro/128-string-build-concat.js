// Building strings with += in a loop, then reading their length and a character.
// features: loop, string, call
// expect: 549968
function buildRow(n) {
  var s = "";
  for (var i = 0; i < n; i++) s += i % 10;
  return s;
}
function main() {
  var t = 0;
  for (var r = 0; r < 5000; r++) {
    var row = buildRow(50 + (r & 15));
    t += row.length + row.charCodeAt(r % 50);
  }
  return t;
}
main();
