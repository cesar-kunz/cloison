// Monomorphic field stores of ints, booleans and objects.
// features: loop, object, new, array, null
// expect: 999975
function V() { this.mark = 0; this.fixed = false; this.by = null; }
function main() {
  var vs = [];
  for (var i = 0; i < 100; i++) vs.push(new V());
  for (var r = 0; r < 10000; r++) {
    for (var i = 0; i < 100; i++) {
      var v = vs[i];
      v.mark = r;
      v.fixed = ((r + i) & 3) == 0;
      v.by = v.fixed ? vs[(i + 1) % 100] : null;
    }
  }
  var n = 0;
  for (var i = 0; i < 100; i++) n += vs[i].mark + (vs[i].fixed ? 1 : 0) + (vs[i].by != null ? 2 : 0);
  return n;
}
main();
