// Compiled keyed loads read a live mapped `arguments` object's elements
// through the runtime (#179): they are copies, and a parameter a closure
// wrote is not in them. A load site warmed on mixed arrays (the generic
// stencils, which check the class and not the shape), on plain objects
// (the shape known at entry, where a mapped object passes for one of an
// unmapped shape), given up on (the runtime's stencils), and with string
// keys, each read 3000 times in a frame whose closure writes the
// parameter: no read may be stale. And `JSON.rawJSON`'s objects, whose
// flag is the same bit, still print raw; a frozen `arguments` object
// keeps its values. What SpiderMonkey and JavaScriptCore give.
// expect: 0 0 0 0 0 {"a":12} {"0":3}3

var out = [];
function rd(x, i) { var s = 0; for (var q = 0; q < 3; q++) s += q; return x[i]; }
var arr = [1, "x", {}];
for (var j = 0; j < 20000; j++) rd(arr, 0);
function f(a) { var k = function (v) { a = v; }; var bad = 0; for (var i = 0; i < 3000; i++) { k(i); if (rd(arguments, 0) !== i) bad++; } return bad; }
out.push(f(0));
function rb(box, i) { var s = 0; for (var q = 0; q < 3; q++) s += q; var x = box.v; return x[i]; }
for (var j = 0; j < 20000; j++) rb({ v: arr }, 0);
function fb(a) { var k = function (v) { a = v; }; var bad = 0; for (var i = 0; i < 3000; i++) { k(i); if (rb({ v: arguments }, 0) !== i) bad++; } return bad; }
out.push(fb(0));
function rp(x, i) { var s = 0; for (var q = 0; q < 3; q++) s += q; return x[i]; }
var po = { 0: 1, 1: "x", 2: {} };
for (var j = 0; j < 20000; j++) rp(po, j % 3);
function fp(a) { var k = function (v) { a = v; }; var bad = 0; for (var i = 0; i < 3000; i++) { k(i); if (rp(arguments, 0) !== i) bad++; } return bad; }
out.push(fp(0));
function rm(x, i) { var s = 0; for (var q = 0; q < 3; q++) s += q; return x[i]; }
var objs = [];
for (var j = 0; j < 20; j++) { var o = {}; o["p" + j] = 1; o[0] = j; objs.push(o); }
for (var j = 0; j < 40000; j++) rm(objs[j % 20], 0);
function fm(a) { var k = function (v) { a = v; }; var bad = 0; for (var i = 0; i < 3000; i++) { k(i); if (rm(arguments, 0) !== i) bad++; } return bad; }
out.push(fm(0));
function rs(x, i) { var s = 0; for (var q = 0; q < 3; q++) s += q; return x[i]; }
for (var j = 0; j < 20000; j++) rs(arr, j % 2 ? 0 : "0");
function fs(a) { var k = function (v) { a = v; }; var bad = 0; for (var i = 0; i < 3000; i++) { k(i); if (rs(arguments, i % 2 ? 0 : "0") !== i) bad++; } return bad; }
out.push(fs(0));
out.push(JSON.stringify({ a: JSON.rawJSON("12") }));
function fz(a) { var k = function (v) { a = v; }; k(3); Object.freeze(arguments); k(4); return JSON.stringify(arguments) + rd(arguments, 0); }
out.push(fz(1));
out.join(" ")
