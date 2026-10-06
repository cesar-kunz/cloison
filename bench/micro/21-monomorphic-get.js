// Monomorphic field loads in a loop over same-shaped objects.
// features: loop, object, new, array
// expect: 29500000
function V(i) { this.value = i; this.mark = i % 5; this.fixed = (i & 1) == 0; }
function main() {
  var vs = [];
  for (var i = 0; i < 100; i++) vs.push(new V(i));
  var n = 0;
  for (var r = 0; r < 10000; r++) {
    for (var i = 0; i < 100; i++) {
      var v = vs[i];
      if (v.mark == (r % 5) || v.fixed) n += v.value;
    }
  }
  return n;
}
main();
