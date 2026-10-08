// Checks in hot code whose failure throws; they never fail.
// toi: `throw` must compile even where it never runs (DeltaBlue's harness throws from `alert`).
// features: loop, string, call, object, new, proto, throw
// expect: 512370976
function fail(what) { throw "check failed: " + what; }
function Cell(v) { this.value = v; this.writes = 0; }
Cell.prototype.set = function (v) {
  if (v != v) fail("NaN");
  this.value = v;
  this.writes++;
};
function check(c, want) {
  if (c.value != want) fail("value " + c.value + ", wanted " + want);
}
function main() {
  var c = new Cell(0), sum = 0;
  for (var i = 0; i < 1000000; i++) {
    c.set(i & 1023);
    check(c, i & 1023);
    sum += c.value;
  }
  return sum + c.writes;
}
main();
