// A variable only a closure names is in the frame the call clears: the
// closure reads `undefined`, not what an earlier call left in its
// register (a function expression's own name shadowed by a `var` too).
// expect: undefined undefined,undefined,undefined undefined function

function leak() { var a = { s: "stale" }, b = a, c = a; return a.s + b.s + c.s; }
var r = [];
leak(); var p1; (function () { var x; p1 = function () { return x; }; })(); r.push(typeof p1());
leak(); var p2; (function () { var x, y, z; p2 = () => [x, y, z].map(v => typeof v).join(); })(); r.push(p2());
leak(); var p3; var f = function n() { var n; p3 = function () { return n; }; }; f(); r.push(typeof p3());
leak(); var p4; var g = function n() { p4 = function () { return typeof n; }; }; g(); r.push(p4());
r.join(" ");
