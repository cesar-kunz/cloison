// Object-or-null fields: tested, overwritten and reset.
// features: loop, object, new, null, array
// expect: 874998
function V() { this.by = null; }
function C(k) { this.k = k; }
function main() {
  var v = new V(), cs = [new C(1), new C(2), new C(3)], n = 0;
  for (var i = 0; i < 500000; i++) {
    var c = cs[i % 3];
    var old = v.by;
    if (old != null) n += old.k;
    v.by = c;
    if ((i & 7) == 0 && v.by == c) v.by = null;
  }
  return n;
}
main();
