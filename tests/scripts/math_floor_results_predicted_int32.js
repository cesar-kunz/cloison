// `Math.floor`, `ceil` and `round` results predicted int32 (checked
// after the call); a call giving a double, NaN or -0 leaves with it, and
// is predicted no more. The expected value is Node's.
// expect: 1349961921417876,89935228932,0,NaN,-8,32

function f(x) { var a = Math.floor(x) + 1, b = Math.ceil(x) | 0, c = Math.round(x) * 2; return a + b + c; }
var s = 0;
for (var i = 0; i < 30000; i++) s += f(i * 0.75);
var t = [f(3e10 + 0.5), f(-0.2), f(NaN), f(-2.5), f("7.5")];
for (var i = 0; i < 30000; i++) s += f(i * 1e6 + 0.25);
[s].concat(t).join();
