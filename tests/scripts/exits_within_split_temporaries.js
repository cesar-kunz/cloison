// Temporaries split off their registers (`jit::split_temps`): exits
// between one's store and its reads (a string, `null` among the
// numbers) leave the interpreter its register as the bytecode had it.
// The expected value is Node's.
// expect: 1054.0532493329365:81.5,1054.0532493329365:81.5,NaN:81.5,1054.0532493329365:81.5,1054.0532493329365:81.5

function mix(a, b, c, n) {
  var s = 0, t = "";
  for (var i = 0; i < n; i++) {
    s = (a[i] * 1.5 + b[i] * 2.25) * (c[i] + 0.5) + s * 0.25;
    t = a[i] - (b[i] + c[i] * 3);
  }
  return s + ":" + t;
}
function fill(n, f) { var r = []; for (var i = 0; i < n; i++) r.push(f(i)); return r; }
var a = fill(200, function (i) { return i * 0.5; });
var b = fill(200, function (i) { return i % 7; });
var c = fill(200, function (i) { return (i % 5) * 1.25; });
var out = [];
for (var k = 0; k < 50; k++) out.push(mix(a, b, c, 200));
b[150] = "x"; c[160] = null; a[170] = "12";
out.push(mix(a, b, c, 200));
b[150] = 3;
for (var k = 0; k < 50; k++) out.push(mix(a, b, c, 200));
out.slice(48, 53).join(",");
