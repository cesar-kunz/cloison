// A dense prefix of N filled after a far key (a[1e9]): the prefix is read
// and written in place while the array is far.
if (typeof N != 'number') var N = 1e5;
function run() {
  var t = Date.now(), s = 0;
  for (var r = 0; r < 1e7 / N; r++) {
    var a = [];
    a[1e9] = 1;
    for (var i = 0; i < N; i++) a[i] = i;
    for (var i = 0; i < N; i++) a[i] = a[i] + 1;
    for (var i = 0; i < N; i++) s += a[i];
    s += a.length;
  }
  print('prefix-far', N, Date.now() - t, s);
}
run();
