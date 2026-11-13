// An empty array's push cache and the megamorphic store table name the
// shapes compiled code moves objects to: a full collection that finds
// them dead (no array of a double left, no object given `x`) drops
// them, and objects made after get their prototype, as V8,
// SpiderMonkey, JavaScriptCore and QuickJS (a zombie's was null).
// expect: true function true 63 1

function pair(a) { return [a]; }
function pairs(n, a) { var r; for (var i = 0; i < n; i++) r = pair(a); return r; }
for (var k = 0; k < 50; k++) pairs(2000, 1.5);
var kinds = [], keep = [];
for (var k = 0; k < 64; k++) { var p = {}; p["k" + k] = k; kinds.push(p); }
function make(k) { return Object.create(kinds[k]); }
function set(o) { o.x = 1; return o; }
function sets(n) { var r; for (var i = 0; i < n; i++) r = set(make(i & 63)); return r; }
for (var k = 0; k < 64; k++) keep.push(make(k));
for (var k = 0; k < 50; k++) sets(2000);
for (var k = 0; k < 2000; k++) { var o = {}; o["p" + k] = k; }
$262.gc();
var a = pairs(1, 1.5), o = sets(64);
[Object.getPrototypeOf(a) === Array.prototype, typeof a.map, Object.getPrototypeOf(o) === kinds[63], o.k63, o.x].join(" ")
