// forEach, indexOf, reduce, slice and sort over N elements every K-th.
if (typeof N != 'number') var N = 1e5;
if (typeof K != 'number') var K = 16;
function run() {
  var a = [];
  for (var i = 0; i < N; i++) a[i * K] = i;
  var t = Date.now(), s = 0;
  for (var r = 0; r < 1e6 / N; r++) {
    a.forEach(function (v) { s += v; });
    s += a.indexOf(N - 1);
    s += a.reduce(function (x, v) { return x + v; }, 0);
    s += a.slice(K * (N >> 1)).length;
  }
  var b = a.slice(); b.sort(function (x, y) { return y - x; }); s += b[0];
  print('methods' + K, N, Date.now() - t, s);
}
run();
