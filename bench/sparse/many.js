// Many small arrays, each with ten elements and one far key: what going
// far costs per array, and its memory.
if (typeof N != 'number') var N = 2000;
function run() {
  var t = Date.now(), s = 0, keep = [];
  for (var r = 0; r < 1e5 / N; r++) {
    keep = [];
    for (var j = 0; j < N; j++) {
      var a = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9];
      a[20000 + j] = j;
      keep.push(a);
    }
    for (var j = 0; j < N; j++) s += keep[j][20000 + j] + keep[j].length;
  }
  print('many', N, Date.now() - t, s);
}
run();
