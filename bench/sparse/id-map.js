// N integer ids from [0, 1e9) used as keys, then looked up: stays far.
if (typeof N != 'number') var N = 1e5;
function run() {
  var ids = [], x = 777;
  for (var i = 0; i < N; i++) { x = (x * 1103515245 + 12345) & 0x7fffffff; ids[i] = x % 1e9; }
  var t = Date.now(), s = 0;
  for (var r = 0; r < 2e6 / N; r++) {
    var a = [];
    for (var i = 0; i < N; i++) a[ids[i]] = i;
    for (var i = 0; i < N; i++) s += a[ids[i]];
  }
  print('id-map', N, Date.now() - t, s);
}
run();
