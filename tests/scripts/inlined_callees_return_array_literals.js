// An array literal's pushes are calls without an inline cache: inlining
// a function returning one, in a function with no inline caches at all
// or with some, doesn't read a cache for them (it read past the caches,
// and crashed).
// expect: 10713435724

function pair(a, b) { return [a, b]; }
function divmod(a, b) { return [(a / b) | 0, a % b]; }
function tagged(o, b) { return [o.k, b]; }
function main() {
  var s = 0, o = {k: 3};
  for (var i = 1; i < 100000; i++) {
    var p = pair(i, 7), q = divmod(i, 7), r = tagged(o, i);
    s += p[0] - p[1] + q[0] - q[1] + r[0] + r[1];
  }
  return s;
}
main();
