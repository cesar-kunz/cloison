// A fixed-length buffer doesn't keep its views: once one is detached
// (`transfer`), compiled code is dropped and its views' elements and
// lengths are read from their buffers (`Vm::views_detached`), not from
// the lengths the views were made with.
// expect: 290000 0 0 60000 3 0

function len(a) { return a.length; }
function rd(ta, i) { return ta[i]; }
function wr(ta, i, v) { ta[i] = v; }
var buf = new ArrayBuffer(32), ta = new Int32Array(buf), arr = [1, 2, 3], s = 0;
for (var n = 0; n < 20000; n++) { wr(ta, n & 7, n & 7); s += rd(ta, n & 7) + len(ta) + len(arr); }
var other = buf.transfer();
var t = 0, l = 0, la = 0;
for (var n = 0; n < 20000; n++) {
  wr(ta, n & 7, 999);
  if (rd(ta, n & 7) !== undefined) t++;
  l += len(ta);
  la += len(arr);
}
[s, t, l, la, new Int32Array(other)[3], ta.byteLength].join(" ");
