// A numeric field that holds int32 values, then doubles after an inexact division.
// features: loop, double, object, new, array
// expect: 4075331.5555752255
function Cell(v) { this.value = v; }
function main() {
  var cells = [];
  for (var i = 0; i < 64; i++) cells.push(new Cell(i));
  var acc = 0;
  for (var r = 0; r < 4000; r++) {
    for (var i = 0; i < 64; i++) {
      var c = cells[i];
      c.value = r < 2000 ? (c.value * 10 + 1000 - 1000) / 10 : (c.value + 1) / 4;
      acc += c.value;
    }
  }
  return acc;
}
main();
