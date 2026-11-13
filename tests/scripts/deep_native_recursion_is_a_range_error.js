// Natives calling natives deeply, with no bytecode frame between (a chain
// of iterator helpers, nested arrays' strings, a proxy of a proxy…), run
// out of stack as a `RangeError`, not by aborting.
// expect: RangeError RangeError RangeError RangeError 101,102

function t(f) { try { return String(f()); } catch (e) { return e.name; } }
var r = [];
r.push(t(() => { var it = [1].values(); for (var i = 0; i < 100000; i++) it = it.map(x => x); return [...it].length; }));
r.push(t(() => { var a = []; for (var i = 0; i < 100000; i++) a = [a]; return String(a).length; }));
r.push(t(() => { var p = function () { return 1; }; for (var i = 0; i < 100000; i++) p = new Proxy(p, {}); return p(); }));
r.push(t(() => { var p = function () { this.a = 1; }; for (var i = 0; i < 100000; i++) p = new Proxy(p, {}); return new p().a; }));
r.push(t(() => { var it = [1, 2].values(); for (var i = 0; i < 100; i++) it = it.map(x => x + 1); return [...it].join(); }));
r.join(" ");
