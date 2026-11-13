// A float array's NaN, whatever its bytes, is NaN to compiled code too:
// its payload doesn't box as another value (an object, an int32, a
// pointer of the program's choosing).
// expect: numbertruetruetrue numbertruetruetrue numbertruetruetrue numbertruetruetrue numbertrue

var f64 = new Float64Array(1), u = new Uint32Array(f64.buffer);
var f32 = new Float32Array(1), u32 = new Uint32Array(f32.buffer);
function rd(n) { var x; for (var i = 0; i < n; i++) x = f64[0]; return x; }
function rd32(n) { var x; for (var i = 0; i < n; i++) x = f32[0]; return x; }
function keep(n) { var a = [0, 0, 0], o = { p: 0 }; for (var i = 0; i < n; i++) { a[i % 3] = f64[0]; o.p = f64[0]; } return [a[0], o.p]; }
var r = [];
[[0xffffffff, 0xffffffff], [0, 0xfffc0000], [0x41414140, 0xffff0000], [1, 0x7ff00000]].forEach(function (w) {
  u[0] = w[0]; u[1] = w[1];
  var v = rd(3000), k = keep(3000);
  r.push(typeof v + (v !== v) + (k[0] !== k[0]) + (k[1] !== k[1]));
});
u32[0] = 0xffffffff;
var v = rd32(3000);
r.push(typeof v + (v !== v));
r.join(" ");
