// new Array(N), then a fill from 0 and a read pass: dense below the
// waste floor, else far until the fill brings it back.
if (typeof N != 'number') var N = 1e6;
function run() {
  var t = Date.now(), s = 0;
  for (var r = 0; r < 2e7 / N; r++) {
    var a = new Array(N);
    for (var i = 0; i < N; i++) a[i] = i;
    for (var i = 0; i < N; i++) s += a[i];
  }
  print('prealloc', N, Date.now() - t, s);
}
run();
