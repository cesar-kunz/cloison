// Typed arrays' elements read and stored at shared sites: each kind with
// the values that convert, sites meeting several kinds and BigInt ones,
// arrays, strings and objects, a site that met an array first, views of a
// transferred or resized buffer, keys out of range or not integers.
// Expected value from V8.
// expect: Int8Array:undefined,0,1,-1,127,-128,-1,0,127,0,1,2,0,0,0,-1,0,-1,0,3,undefined Uint8Array:undefined,0,1,255,127,128,255,0,127,0,1,2,0,0,0,255,0,255,0,3,undefined Uint8ClampedArray:undefined,0,1,0,128,128,255,255,0,0,2,2,0,0,255,255,255,0,255,3,undefined Int16Array:undefined,0,1,-1,127,128,255,256,-129,0,1,2,0,0,0,-1,0,-1,-7168,3,undefined Uint16Array:undefined,0,1,65535,127,128,255,256,65407,0,1,2,0,0,0,65535,0,65535,58368,3,undefined Int32Array:undefined,0,1,-1,127,128,255,256,-129,0,1,2,0,0,0,65535,-2147483648,2147483647,1410065408,3,undefined Uint32Array:undefined,0,1,4294967295,127,128,255,256,4294967167,0,1,2,0,0,0,65535,2147483648,2147483647,1410065408,3,undefined Float32Array:undefined,0,1,-1,127.5,128,255.5,256,-129,0.5,1.5,2.5,-0,NaN,Infinity,65535,2147483648,-2147483648,10000000000,3.141590118408203,undefined Float64Array:undefined,0,1,-1,127.5,128,255.5,256,-129,0.5,1.5,2.5,-0,NaN,Infinity,65535,2147483648,-2147483649,10000000000,3.14159,undefined Float16Array:undefined,0,1,-1,127.5,128,255.5,256,-129,0.5,1.5,2.5,-0,NaN,Infinity,Infinity,Infinity,-Infinity,Infinity,3.140625,undefined 9/8 0.25/0.5 1n/-2n -7/7 3n/18446744073709551615n 1/2 a/b x/y 8 8.75 8n 8 8n 8.75 b 8.75 60 2.12199579097e-313 undefined 0 60 undefined 1516 undefined 1006 0 16 28888047 7 7 undefined 0 0 5/undefined 1.5/undefined 7n/undefined 3/undefined 7/undefined 1/undefined 9/undefined 5/undefined 1.5/undefined 7n/undefined 3/undefined 7/undefined 1/undefined undefined/undefined
var out = [];
function S(v) { return typeof v === "bigint" ? v + "n" : Object.is(v, -0) ? "-0" : String(v); }
function get(a, i) { return a[i]; }
function put(a, i, v) { a[i] = v; }
function sum(a) { var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return s; }
var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array, Float16Array];
var vals = [0, 1, -1, 127.5, 128, 255.5, 256, -129, 0.5, 1.5, 2.5, -0, NaN, Infinity, 65535, 2147483648, -2147483649, 1e10, 3.14159];
for (var r = 0; r < 3; r++) for (var k = 0; k < kinds.length; k++) {
  var a = new kinds[k](vals.length);
  for (var i = 0; i < vals.length; i++) put(a, i, vals[i]);
  var row = [];
  for (var i = -1; i <= vals.length; i++) row.push(S(get(a, i)));
  if (r == 2) out.push(kinds[k].name + ":" + row.join(","));
}
var big = [new BigInt64Array([1n, -2n]), new BigUint64Array([3n, 2n ** 64n - 1n])];
var mix = [new Uint8Array([9, 8]), new Float64Array([0.25, 0.5]), big[0], new Int32Array([-7, 7]), big[1], [1, 2], "ab", { 0: "x", 1: "y" }];
for (var r = 0; r < 4; r++) for (var j = 0; j < mix.length; j++) if (r == 3) out.push(S(get(mix[j], 0)) + "/" + S(get(mix[j], 1)));
for (var r = 0; r < 4; r++) for (var j = 0; j < mix.length; j++) { try { put(mix[j], 1, j == 2 || j == 4 ? 5n + BigInt(r) : 5.75 + r); if (r == 3) out.push(S(get(mix[j], 1))); } catch (e) { out.push(e.constructor.name); } }
var buf = new ArrayBuffer(16), v = new Int32Array(buf), d = new Float64Array(buf);
for (var i = 0; i < 4; i++) put(v, i, i * 10);
out.push(S(sum(v)), S(get(d, 0)));
var moved = buf.transfer();
out.push(S(get(v, 0)), S(v.length), S(sum(new Int32Array(moved))));
put(v, 0, 1); out.push(S(get(v, 0)));
var rab = new ArrayBuffer(8, { maxByteLength: 16 }), rv = new Uint8Array(rab);
for (var i = 0; i < 8; i++) put(rv, i, i + 250);
out.push(S(sum(rv))); rab.resize(4); out.push(S(get(rv, 5)), S(sum(rv))); rab.resize(16); out.push(S(get(rv, 12)), S(rv.length));
var t = new Float64Array(1000), u = new Uint16Array(1000);
for (var i = 0; i < 1000; i++) { t[i] = i / 4; u[i] = i * 77; }
var s = 0; for (var i = 0; i < 1000; i++) s += t[i] + u[i] + (t[i + 1000] === undefined ? 1 : 0);
out.push(S(s));
var obj = new Uint8Array(4); put(obj, "1", 7); put(obj, 1.5, 9); out.push(S(get(obj, 1)), S(get(obj, "1")), S(get(obj, 1.5)), S(obj[-0]));
out.push(S(get(new Uint8Array(2), { valueOf: function () { return 1; }, toString: function () { return "1"; } })));
// A site that met a plain array first: its cache keeps the array's shape.
function first(a, i) { return a[i]; }
var later = [[4, 5], new Float64Array([0.5, 1.5]), new BigUint64Array([6n, 7n]), new Int8Array([-3, 3]), [6, 7], new Uint8ClampedArray([255, 1])];
var dv = new Uint16Array(new ArrayBuffer(4)); dv[1] = 9; later.push(dv);
for (var r = 0; r < 3; r++) { if (r == 2) dv.buffer.transfer(); for (var j = 0; j < later.length; j++) if (r > 0) out.push(S(first(later[j], 1)) + "/" + S(first(later[j], 2))); }
out.join(" ");
