// In-place insertion sort of an int array, re-shuffled each round.
// features: loop, array, call
// expect: 44750
function insertionSort(a) {
  for (var i = 1; i < a.length; i++) {
    var x = a[i], j = i - 1;
    while (j >= 0 && a[j] > x) { a[j + 1] = a[j]; j--; }
    a[j + 1] = x;
  }
}
function shuffle(a, seed) {
  for (var i = a.length - 1; i > 0; i--) {
    seed = (seed * 1103 + 12345) & 0xffff;
    var j = seed % (i + 1), t = a[i];
    a[i] = a[j]; a[j] = t;
  }
  return seed;
}
function main() {
  var a = [];
  for (var i = 0; i < 200; i++) a.push(i);
  var seed = 1, s = 0;
  for (var r = 0; r < 500; r++) {
    seed = shuffle(a, seed);
    insertionSort(a);
    s += a[r % 200];
  }
  return s;
}
main();
