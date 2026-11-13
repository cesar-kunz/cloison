// `Math.max` and `Math.min` of two compiled inline: ints, doubles, NaN,
// -0 and +0, and arguments to convert (a string, an object's `valueOf`),
// for which the code leaves. The expected value is Node's.
// expect: 59902725580,99,60000,2/1/0.5/1,2.5/-1/0.4/-1,0/0/Infinity/-Infinity,0/0/Infinity/-Infinity,NaN/NaN/NaN/NaN,NaN/NaN/NaN/NaN,3/3/0.3333333333333333/0.3333333333333333,5/2/0.2/0.5,4/1/0.25/1,1e+308/-Infinity/1e-308/0,7.5/7/0.13333333333333333/0.14285714285714285

function f(a, b) { return Math.max(a, b) + "/" + Math.min(a, b) + "/" + (1 / Math.max(a, b)) + "/" + (1 / Math.min(a, b)); }
function g(a, b) { var s = 0; for (var i = 0; i < 1000; i++) s += Math.max(a + i, b) - Math.min(a, b - i); return s; }
var s = 0;
for (var i = 0; i < 30000; i++) s += g(i % 7, 3.5) + g(1.25, i % 5);
var o = { valueOf: function () { return 4; } };
var t = [f(1, 2), f(2.5, -1), f(-0, 0), f(0, -0), f(NaN, 1), f(1, NaN), f(3, 3), f("5", 2), f(o, 1), f(-Infinity, 1e308), f(7, 7.5)];
var q = { m: 0 }; for (var i = 0; i < 20000; i++) q.m = Math.max(q.m, i % 100);
var r = 0; for (var i = 0; i < 20000; i++) r += [f(1, 2), f(-0, 0), f(o, 1)].length;
[s, q.m, r].concat(t).join();
