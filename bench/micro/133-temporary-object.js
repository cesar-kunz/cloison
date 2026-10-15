// A temporary object built and consumed inside one function: it never escapes.
// features: loop, object, new, call
// expect: 14000000
function Range(lo, hi) { this.lo = lo; this.hi = hi; }
function overlapLen(a, b, c, d) {
  var r = new Range(a > c ? a : c, b < d ? b : d);
  return r.hi > r.lo ? r.hi - r.lo : 0;
}
function main() {
  var s = 0;
  for (var i = 0; i < 2000000; i++) s += overlapLen(i & 31, (i & 31) + 10, i & 15, (i & 15) + 20);
  return s;
}
main();
