// An object key is converted once for a read-modify-write of `o[k]`
// (`+=`, `++`, `??=`), even nested in calls in its value, and once for
// each of two statements reading and writing `o[k]`.
// expect: 1 p1 1 {"p1":6} 1 {"p1":3} 2 p2 2 {"p2":2} 3 p3,p2,p1

var n = 0, k = { toString() { n++; return "p" + n; } }, a = [];
n = 0; var o = {}; o[k] += 1; a.push(n, Object.keys(o).join());
n = 0; var o3 = { p1: 5 }; o3[k]++; a.push(n, JSON.stringify(o3));
n = 0; var o4 = {}; o4[k] ??= 3; a.push(n, JSON.stringify(o4));
n = 0; var o2 = {}; var t = o2[k]; o2[k] = 1; a.push(n, Object.keys(o2).join());
n = 0; var o6 = { p1: 1 }; try { o6[k] += (function () { throw 1; })(); } catch (e) {} var o7 = {}; o7[k] = 2; a.push(n, JSON.stringify(o7));
function rec(o, d) { if (d) o[k] += rec(o, d - 1); return 1; }
n = 0; var o8 = {}; rec(o8, 3); a.push(n, Object.keys(o8).join());
a.join(" ");
