// A fill of [0, N) in a random order: the density grows from 0 to 1.
if (typeof N != 'number') var N = 1e6;
function run() {
  var p = [], x = 12345;
  for (var i = 0; i < N; i++) p[i] = i;
  for (var i = N - 1; i > 0; i--) {
    x = (x * 1103515245 + 12345) & 0x7fffffff;
    var j = x % (i + 1), v = p[i]; p[i] = p[j]; p[j] = v;
  }
  var t = Date.now(), s = 0;
  for (var r = 0; r < 1e7 / N; r++) {
    var a = [];
    for (var i = 0; i < N; i++) a[p[i]] = i;
    for (var i = 0; i < N; i++) s += a[i];
  }
  print('random', N, Date.now() - t, s);
}
run();
