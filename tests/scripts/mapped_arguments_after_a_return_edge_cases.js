// #163's edge cases: both ways and again, a parameter written in its frame
// first, some parameters captured, more arguments than parameters and
// fewer, a frame unwound instead of returning, parameters a direct
// `eval`'s code captures (in the scope object), indices unmapped after
// the return (deleted, made non-writable or an accessor, frozen) or kept
// (made non-enumerable, redefined, sealed), the object read through
// natives, spread and as a prototype, properties added before and after
// the return (dictionary mode), a read site whose cache saw copied
// objects of the mapped shape (whose elements it read for a live one's
// stale ones before), an object made not extensible or sealed in its
// frame, keys and descriptors of those kept, and many kept across
// collections. What SpiderMonkey and JavaScriptCore give.
// expect: 2 2 5 3,2|3|2 5,6 9,8 ,1,,6,3 4,7,1 2 3 2 3 5 4,5|7|5 6,7|3 6,7|g 6 4,2 8,9|6 6 2,7 {"0":10,"1":11} 10,11 10,11 11 11 [object Arguments] 3,4 6 5 5,8 5 false 0,1 {"value":5,"writable":true,"enumerable":true,"configurable":true} 6 2 false 5 true false false {"value":5,"writable":true,"enumerable":true,"configurable":false} 7 7 8 true 0,1,2 0,1,2,length,callee {"0":1,"1":9,"2":3} 5 0,1,2 0

var out = [];
// The issue's: each side sees the other's writes after the return.
function f(a) { return [function () { return a; }, arguments]; }
var r = f(1);
r[1][0] = 2;
out.push(r[0]() + " " + r[1][0]);
function h(a) { var k = function (v) { a = v; }; return [k, arguments]; }
var q = h(1);
q[0](5);
out.push(q[1][0]);
// Both ways, more than once, a parameter written in the frame first.
function d(a, b) { a = 3; return [function () { return a + "," + b; }, arguments, function (v) { a = v; b = v + 1; }]; }
var x = d(1, 2);
out.push(x[0]() + "|" + x[1][0] + "|" + x[1][1]);
x[2](5); out.push(x[1][0] + "," + x[1][1]);
x[1][0] = 9; x[1][1] = 8; out.push(x[0]());
// Only some parameters captured; more arguments than parameters; fewer.
function s(a, b) { return [function (v) { b = v; return b; }, arguments]; }
var sr = s(1, 2, 3); sr[1][1] = 5; sr[1][2] = 6;
out.push([sr[0](), sr[1][0], sr[1][1], sr[1][2], sr[1].length].join());
var sr2 = s(1); sr2[1][1] = 7; out.push([sr2[0](4), sr2[1][1], sr2[1].length].join());
// Unwound, not returned.
function u(a) { var k = [function () { return a; }, function (v) { a = v; }]; k.push(arguments); throw k; }
try { u(1); } catch (k) { k[2][0] = 2; out.push(k[0]()); k[1](3); out.push(k[2][0]); }
// Captured by a direct eval's code, or with one in the function.
function e(a) { return [eval("(function () { return a; })"), arguments, eval("(function (v) { a = v; })")]; }
var er = e(1); er[1][0] = 2; out.push(er[0]()); er[2](3); out.push(er[1][0]);
function e2(a) { eval(""); return [function () { return a; }, arguments]; }
var er2 = e2(1); er2[1][0] = 5; out.push(er2[0]());
// Unmapped after the return: deleted, redefined, frozen.
var dl = d(1, 2); delete dl[1][0]; dl[1][0] = 7; dl[2](4); out.push(dl[0]() + "|" + dl[1][0] + "|" + dl[1][1]);
var nw = d(1, 2); Object.defineProperty(nw[1], 0, { writable: false }); nw[2](6); out.push(nw[0]() + "|" + nw[1][0]);
var ac = d(1, 2); Object.defineProperty(ac[1], 0, { get: function () { return "g"; } }); ac[2](6); out.push(ac[0]() + "|" + ac[1][0]);
var ne = d(1, 2); Object.defineProperty(ne[1], 0, { enumerable: false }); ne[2](6); out.push(ne[1][0]);
var df = d(1, 2); Object.defineProperty(df[1], 0, { value: 4 }); out.push(df[0]());
var fz = d(1, 2); fz[2](6); Object.freeze(fz[1]); fz[2](8); out.push(fz[0]() + "|" + fz[1][0]);
var se = d(1, 2); Object.seal(se[1]); se[2](6); out.push(se[1][0]); se[1][0] = 2; out.push(se[0]());
// Read through the object's other ways.
var ot = d(1, 2); ot[2](10);
out.push(JSON.stringify(ot[1]) + " " + [].slice.call(ot[1]) + " " + [...ot[1]] + " " + Math.max.apply(null, ot[1]) + " " +
  Object.getOwnPropertyDescriptor(ot[1], 1).value + " " + Object.prototype.toString.call(ot[1]));
var pr = d(1, 2); var child = Object.create(pr[1]); pr[2](3); out.push(child[0] + "," + child[1]);
// Properties added, many (dictionary mode), before and after the return.
function lv(a) { for (var i = 0; i < 40; i++) arguments["p" + i] = i; delete arguments.p3; return [function () { return a; }, arguments]; }
var y = lv(1); y[1][0] = 6; delete y[1].p5; out.push(y[0]());
var z = d(1, 2); for (var i = 0; i < 40; i++) z[1]["p" + i] = i; delete z[1].p3; delete z[1].p7; z[2](5); out.push(z[1][0]);
// A read site warmed with copied objects of the mapped shape, then a
// live one and one still mapped after its return.
function rd(o) { return o[0]; }
function mk(a) { a = 2; arguments.length; return arguments; }
for (var i = 0; i < 300; i++) rd(mk(i));
function live(a) { var k = function () { a = 5; }; k(); return rd(arguments); }
var c = d(1, 2); c[2](8);
out.push(live(1) + "," + rd(c[1]));
// Made not extensible or sealed in the frame; some elements kept.
function pe(a, b) { Object.preventExtensions(arguments); return [function (v) { if (v !== undefined) a = v; return a; }, arguments]; }
var p = pe(1, 2); p[0](5); out.push(p[1][0], Object.isExtensible(p[1]), Object.keys(p[1]).join(), JSON.stringify(Object.getOwnPropertyDescriptor(p[1], 0)));
p[1][0] = 6; out.push(p[0]()); p[1][3] = 1; out.push(p[1].length, 3 in p[1]);
function sl(a, b) { Object.seal(arguments); return [function (v) { if (v !== undefined) a = v; return a; }, arguments]; }
var q = sl(1, 2); q[0](5); out.push(q[1][0], Object.isSealed(q[1]), Object.isFrozen(q[1]), delete q[1][0], JSON.stringify(Object.getOwnPropertyDescriptor(q[1], 0)));
q[1][0] = 7; out.push(q[0]()); Object.freeze(q[1]); q[0](8); out.push(q[1][0], q[0](), Object.isFrozen(q[1]));
function ks(a, b, c) { return [function (v) { if (v !== undefined) b = v; return b; }, arguments]; }
var k = ks(1, 2, 3); k[0](9); out.push(Object.keys(k[1]).join(), Object.getOwnPropertyNames(k[1]).join(), JSON.stringify(k[1]));
var kk = ks(1, 2, 3); Object.defineProperty(kk[1], 1, { value: 4, enumerable: true }); kk[0](5); out.push(kk[1][1], Object.keys(kk[1]).join());
// Many, hot, kept across collections.
var keep = [];
for (var i = 0; i < 500; i++) keep.push(f({ i: i }), e2({ i: i }));
var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i: i });
var bad = 0;
for (var i = 0; i < keep.length; i++) {
  var k = keep[i];
  k[1][0] = { n: i };
  if (k[0]().n === undefined) bad++;
}
out.push(bad);
out.join(" ")
