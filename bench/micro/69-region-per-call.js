// A function that allocates many objects and returns only a number, called repeatedly.
// cloison: the region rule: no internal object in or out, so the bump pointer resets on return.
// features: loop, call, object, new, array
// expect: 204982704
function P(x, y) { this.x = x; this.y = y; }
function work(n) {
  var ps = [];
  for (var i = 0; i < n; i++) ps.push(new P(i, n - i));
  var s = 0;
  for (var i = 0; i < ps.length; i++) s += ps[i].x * ps[i].y;
  return s;
}
function main() {
  var total = 0;
  for (var r = 0; r < 1000; r++) total = (total + work(300)) | 0;
  return total;
}
main();
