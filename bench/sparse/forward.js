// Appends from 0: a[i] = i for i < N, then a read pass. Never sparse: a
// change in the density settings must not move it.
if (typeof N != 'number') var N = 1e6;
function run() {
  var t = Date.now(), s = 0;
  for (var r = 0; r < 2e7 / N; r++) {
    var a = [];
    for (var i = 0; i < N; i++) a[i] = i;
    for (var i = 0; i < N; i++) s += a[i];
  }
  print('forward', N, Date.now() - t, s);
}
run();
