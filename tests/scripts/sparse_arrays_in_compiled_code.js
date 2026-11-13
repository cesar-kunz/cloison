// Compiled code on an array that goes far in a call: its `length` (the
// known-class read too, past 2^31 as well), `push`, `pop` and appends
// leave it to the runtime.
// expect: 3000000007 3000000001 100000001 100000002 9 9 100000001 100000002 7 100000002 2147483649 number 45 3000001 11

function goFar(a, i) { a[i] = 1; }
function len(a) { return a.length; }
function lens(a, k) { var s = 0; for (var j = 0; j < 3; j++) { s += a.length; if (j == 1) goFar(a, k); } return s; }
function push(a, v) { return a.push(v); }
function pop(a) { return a.pop(); }
function app(a, v) { a[a.length] = v; return a.length; }
var out = [];
for (var i = 0; i < 20000; i++) { var d = [1, 2, 3]; len(d); lens(d, 3); push(d, 4); pop(d); app(d, 5); }
var a = [1, 2, 3]; out.push(lens(a, 3e9), a.length);
var b = [1, 2, 3]; goFar(b, 1e8);
out.push(len(b), push(b, 9), b[1e8 + 1], pop(b), b.length, app(b, 7), b[1e8 + 1], b.length);
var c = [1, 2]; c[2 ** 31] = 0; out.push(len(c), typeof len(c));
var e = [0]; e[3e6] = 1; var s = 0; for (var k = 0; k < 10; k++) { e[k] = k; s += e[k]; }
out.push(s, e.length, Object.keys(e).length);
out.join(' ');
