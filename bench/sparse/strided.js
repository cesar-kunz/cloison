// Every K-th index, N of them, then reads of them: density 1/K, the
// region around the settings' line.
if (typeof N != 'number') var N = 1e5;
if (typeof K != 'number') var K = 16;
function run() {
  var t = Date.now(), s = 0;
  for (var r = 0; r < 1e7 / N; r++) {
    var a = [];
    for (var i = 0; i < N; i++) a[i * K] = i;
    for (var i = 0; i < N; i++) s += a[i * K];
  }
  print('strided' + K, N, Date.now() - t, s);
}
run();
