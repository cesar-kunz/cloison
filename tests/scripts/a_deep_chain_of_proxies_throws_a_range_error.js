// A chain of proxies forwarding `get` too deep for the stack: a
// RangeError, not a native stack overflow.
// expect: RangeError,2

var p = { x: 1 }; for (var i = 0; i < 1000000; i++) p = new Proxy(p, { has(t, k) { return k in t; } });
var q = { x: 2 }; for (var i = 0; i < 100; i++) q = new Proxy(q, {});
var r; try { r = 'x' in p; } catch (e) { r = e.name; }
r + ',' + q.x;
