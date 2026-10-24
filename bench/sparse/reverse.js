// A fill from N - 1 down to 0: the first store is far, the array comes
// back to dense on the way.
if (typeof N != 'number') var N = 1e6;
function run() {
  var t = Date.now(), s = 0;
  for (var r = 0; r < 2e7 / N; r++) {
    var a = [];
    for (var i = N - 1; i >= 0; i--) a[i] = i;
    for (var i = 0; i < N; i++) s += a[i];
  }
  print('reverse', N, Date.now() - t, s);
}
run();
