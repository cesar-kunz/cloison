// A call site whose callee was `f.call` (its cache `VIA_CALL`) and then a
// leaf native, `Math.max`, called directly: the cache no longer says
// `VIA_CALL` (as the general path leaves it), so compiled code calls each
// as it is. Expected value from V8.
// expect: 24756

function g(x) { return x + 1; }
g.m = Function.prototype.call;
var o2 = { m: Math.max };
function site(o, a, b) { return o.m(a, b); }
var s = site(g, 1, 5);
for (var i = 0; i < 3000; i++) s += site(o2, i & 7, 3);
for (var i = 0; i < 3000; i++) s += site(i & 1 ? g : o2, i & 7, 3);
s
