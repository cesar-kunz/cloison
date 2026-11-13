// Shifts by a constant of 0 mod 32 of a value not known to be a number:
// checked to be an int32, then nothing (there is no `sar_smi_i_0`).
// expect: 150000,42949673010000

function f(a) {
  var s = 0;
  for (var i = 0; i < 20000; i++) {
    var x = a[i & 1];
    s += (x >> 0) + (x << 32) + (x >> 64) + (x >>> 0) + (x << 65536);
  }
  return s;
}
f([1, "2"]) + "," + f([-3, "4.5"]);
