// Arrays far apart are sparse (`far`, docs/sparse-arrays.md): an index
// or a length up to 2^32 - 1 takes memory for the elements present, and
// the length, keys, deletes, truncation, freezing, attributes and the
// fill back to dense agree with SpiderMonkey and JSC.
// expect: 134217729 1  true 134217728 4294967295 big 4294967295 0,1,4294967294 1000000000 false 3 1000001 0,1,2,500,1000000 1000001 0,1,2,500 500 400 0,1,2 1  100000001 1000001 5 999999 2000001 x 123 10000002 2 1 10000000 4294967295 RangeError 1000000001 9 1000000001 7

var out = [];
var x = []; x[2 ** 27] = 1;
out.push(x.length, x[2 ** 27], x[5], 2 ** 27 in x, Object.keys(x).join());
var y = []; y[4294967294] = "big"; out.push(y.length, y[4294967294]);
y[0] = 1; y[1] = 2; out.push(y.length, Object.keys(y).join());
var z = new Array(1e9); out.push(z.length, 0 in z);
z.length = 3; out.push(z.length);
var w = [1, 2, 3]; w[1e6] = 4; w[500] = 5; out.push(w.length, Object.keys(w).join());
delete w[1e6]; out.push(w.length, Object.keys(w).join(), w.indexOf(5));
w.length = 400; out.push(w.length, Object.keys(w).join());
var f = []; f[1e8] = 1; Object.freeze(f); f[1e8] = 2; f[3] = 3; out.push(f[1e8], f[3], f.length);
var r = []; for (var i = 1e6; i >= 0; i--) r[i] = i; out.push(r.length, r[5], r[999999]);
var p = []; p[2e6] = 'x'; for (var i = 0; i < 2e6; i++) p[i] = i; out.push(p.length, p[2e6], p[123]);
var q = []; q[1e7] = 1; q.push(2); out.push(q.length, q[1e7 + 1]);
var s = []; s[1e7] = 1; out.push(s.pop(), s.length);
var t = []; t.length = 4294967295; out.push(t.length);
try { t.push(1); } catch (e) { out.push(e.constructor.name); }
var u = []; u[1e8] = 7; Object.defineProperty(u, 1e9, { value: 9, writable: false });
out.push(u.length, u[1e9]); u.length = 5e8; out.push(u.length, u[1e8]);
out.join(' ');
