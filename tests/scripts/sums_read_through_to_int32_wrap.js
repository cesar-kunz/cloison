// Int32 sums and products read only through ToInt32 within their block
// wrap (`jit::truncated`): sums past 2^31 and 2^32 into `| 0`, `>> 0`,
// `>>> 3`, `&` and `^`, products into a sum (`-0 * 0`'s -0 is 0 there); not
// a sum read exactly after its block or loop (`after`, `carried`), by a
// `catch`, by a closure or through `arguments`, nor one a double is added
// to. The expected value is what V8, SpiderMonkey and JavaScriptCore print.
// expect: 3540000;1958430000;5070000;-1142105632;60000;0/4294967264;-480000;-959840/4294967264;-960000/4294967264;-32/4294967264;-960000

var big = 0x7ffffff0, N = 30000;
function chains(a, b) { var r = 0; for (var i = 0; i < N; i++) { var t = a + b; var u = t + a; var v = u - b; var w = v + t; r = (r + (w | 0) + (u >> 0) + (v >>> 3) + (t & 0xffff) + (w ^ 0x55)) | 0; } return r; }
function prods(a, b) { var r = 0; for (var i = 0; i < N; i++) { var p = a * b; var q = p + a; r = (r + (q | 0)) | 0; var y = ((-a) * 0) >> 0; r = (r + y) | 0; } return r; }
function after(a, b) { var t = 0, r = 0; for (var i = 0; i < N; i++) { t = a + b; r = (r + (t & 7)) | 0; } return r + "/" + t; }
function carried(d) { var x = 0; for (var i = 0; i < N; i++) { x += d; } return x | 0; }
function catcher(a, b) { var r = 0, t = 0; for (var i = 0; i < N; i++) { try { t = a + b; if (i == N - 5) throw 1; r = (r + (t | 0)) | 0; } catch (e) { return r + "/" + t; } } }
function closure(a, b) { var r = 0, t = 0; var g = function () { return t; }; for (var i = 0; i < N; i++) { t = a + b; r = (r + (t | 0)) | 0; } return r + "/" + g(); }
function args(a, b) { var r = 0; for (var i = 0; i < N; i++) { a = a + b; r = (r + (a | 0)) | 0; a = a - b; } a = a + b; var x = a | 0; return x + "/" + arguments[0]; }
function dbl(a, b) { var r = 0; for (var i = 0; i < N; i++) { var t = a + b; r = (r + ((t + 0.5) | 0)) | 0; } return r; }
[chains(3, 4), chains(big, big), chains(-big, -big), prods(0x12345, 0x54321), prods(-0x7fff, 0x10001), after(big, big), carried(big), catcher(big, big), closure(big, big), args(big, big), dbl(big, big)].join(";");
