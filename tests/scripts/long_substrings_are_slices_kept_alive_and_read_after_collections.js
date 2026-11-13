// Long substrings of a flat string whose units are in a large cell are
// slices of it (`Vm::slice_string`): their `elems` point into the
// parent's units, which they keep alive. Slices of slices take the root
// as their parent; a parent no longer referenced lives on through them;
// property keys, concatenation, search and case mapping read their
// units. Run under `CLOISON_GC_VERIFY=64`, `CLOISON_GC_EVACUATE=all` and
// `CLOISON_GC_STRESS=1` too: a slice whose parent was freed or moved
// reads other units.
// expect: 4990,75,true,29000,18000,9000,5000,true,true,true,true,1199,true,1,18000,14000,1201,23,7801,-32,32,316,3619,true,true

function make(n, seed) { var a = []; for (var i = 0; i < n; i++) a.push(String.fromCharCode(97 + (i * seed) % 26)); return a.join(''); }
function sum(s) { var h = 0; for (var i = 0; i < s.length; i++) h = (h * 31 + s.charCodeAt(i)) | 0; return h; }
var out = [], keep = [];
var q = ""; for (var i = 0; i < 6000; i++) q += String.fromCharCode(65 + i % 26);
q.charCodeAt(0); var qs = q.slice(10, 5000); out.push(qs.length, qs.charCodeAt(0), qs === q.substring(10, 5000));
var p = make(30000, 7);
var whole = sum(p);
var a = p.substring(1000), b = a.slice(2000, 20000), c = b.substr(5, 9000), d = p.slice(-5000);
keep.push(a, b, c, d);
out.push(a.length, b.length, c.length, d.length, sum(c) === sum(p.slice(3005, 12005)), b === p.slice(3000, 21000));
p = null;
var junk = [];
for (var r = 0; r < 200; r++) { junk = []; for (var k = 0; k < 2000; k++) junk.push({ k: k, s: 'j' + k }); }
var e = c.slice(100, 3000), f = e.substring(1, 1200);
out.push(sum(a.slice(2005, 11005)) === sum(c), e === c.slice(100, 3000), f.length, sum(f) === sum(a.slice(2106, 3305)));
var o = {}; o[b] = 1; out.push(o[a.slice(2000, 20000)], Object.keys(o)[0].length);
out.push((c + d).length, JSON.stringify(f).length, c.indexOf(f), c.replace(f, '').length, f.toUpperCase().charCodeAt(0) - f.charCodeAt(0));
var m = make(20000, 3);
while (m.length >= 64) { keep.push(m.slice(0, 2)); m = m.substr(64); }
out.push(m.length, keep.length, sum(keep[keep.length - 1]));
for (var r = 0; r < 100; r++) { junk = []; for (var k = 0; k < 2000; k++) junk.push([k]); }
out.push(sum(a) === sum(make(30000, 7).substring(1000)), sum(d) === sum(make(30000, 7).slice(25000)));
out.join()
