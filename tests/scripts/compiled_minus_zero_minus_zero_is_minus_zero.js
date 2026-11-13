// Compiled `x - 0` of a double is `x`: -0 stays -0 (as `x + 0` wouldn't),
// in a register or not.
// expect: true false -Infinity,Infinity,NaN,0,0.4 -2999.5

function k(n, x) { for (var i = 0; i < n; i++) x = x - 0; return Object.is(x, -0); }
function k2(n) { var a = [-0, 0, NaN, -Infinity, 2.5], o = []; for (var i = 0; i < n; i++) for (var j = 0; j < 5; j++) o[j] = 1 / (a[j] - 0); return o.join(); }
function k3(n, x) { for (var i = 0; i < n; i++) x = x - 1; return x; }
[k(3000, -0), k(3000, 0), k2(2000), k3(3000, 0.5)].join(" ");
