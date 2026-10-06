// Removing every occurrence of an element by in-place compaction, then popping the tail.
// features: loop, array, object, new
// expect: 65500
function E(k) { this.k = k; }
function removeAll(elms, elm) {
  var index = 0, skipped = 0;
  for (var i = 0; i < elms.length; i++) {
    var value = elms[i];
    if (value != elm) { elms[index] = value; index++; }
    else skipped++;
  }
  for (var i = 0; i < skipped; i++) elms.pop();
}
function main() {
  var es = [new E(0), new E(1), new E(2), new E(3)], total = 0;
  for (var r = 0; r < 2000; r++) {
    var a = [];
    for (var i = 0; i < 64; i++) a.push(es[i & 3]);
    removeAll(a, es[r & 3]);
    removeAll(a, es[(r + 1) & 3]);
    total += a.length + a[0].k;
  }
  return total;
}
main();
