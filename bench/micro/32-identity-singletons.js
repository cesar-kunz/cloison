// Singletons stored on a constructor, compared by identity.
// features: loop, object, new, global, array
// expect: 875000
function S(v) { this.v = v; }
S.REQUIRED = new S(0);
S.STRONG = new S(1);
S.NORMAL = new S(2);
S.WEAKEST = new S(3);
function main() {
  var all = [S.REQUIRED, S.STRONG, S.NORMAL, S.WEAKEST], n = 0;
  for (var i = 0; i < 500000; i++) {
    var s = all[i & 3];
    if (s == S.REQUIRED) n += 1;
    if (s != S.WEAKEST) n += 2;
  }
  return n;
}
main();
