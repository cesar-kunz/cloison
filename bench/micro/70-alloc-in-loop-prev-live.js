// Allocating in a loop while the previous instance is still live (`prev = v`).
// cloison: two live instances of one site: rules out scalar replacement.
// features: loop, object, new, null
// expect: 44551000
function V(i, prev) { this.value = i; this.prev = prev; }
function main() {
  var total = 0;
  for (var r = 0; r < 1000; r++) {
    var prev = null;
    for (var i = 0; i < 300; i++) {
      var v = new V(i, prev);
      if (prev != null) total += prev.value;
      prev = v;
    }
  }
  return total;
}
main();
