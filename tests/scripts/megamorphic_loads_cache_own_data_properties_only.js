// A load whose four ways are taken probes the interpreter's megamorphic
// cache (`Vm::interp_megas`), of own data properties by shape and name.
// Prototype properties, getters (own and inherited), a deleted or
// redefined property, a dictionary object, a proxy and primitives still
// take the general path; the cache is cleared when shapes are freed, so
// shapes made anew at the same addresses aren't taken for the old ones
// (run under `CLOISON_GC_STRESS=1` and `CLOISON_GC_FULL=1` too).
// Expected values from V8 and SpiderMonkey.
// expect: 78 ; a5 ; protoa0protoa1protoa2protoa3pr ; changed ; a3 ; getter0,a0,getter1,a1,getter2,a2 ;  ; a1 ; set ; now a getter ; dict ; dict ; trap x ;  ;  ; undefined ; 30 ; 40 ; 40 ; z0z1z2z3z4z5z6z7z8z9

var out = [];
function get(o) { return o.x; }
function shapes(n, tag) { var a = []; for (var k = 0; k < n; k++) { var o = {}; o['p' + tag + k] = k; o.x = tag + k; a.push(o); } return a; }
var os = shapes(12, 'a'), s = '';
for (var r = 0; r < 3; r++) for (var i = 0; i < os.length; i++) s += get(os[i]);
out.push(s.length, get(os[5]));
var P = { x: 'proto' }, ch = []; for (var k = 0; k < 8; k++) { var o = Object.create(P); o['q' + k] = 1; ch.push(o); }
s = ''; for (var r = 0; r < 2; r++) for (var i = 0; i < 8; i++) s += get(ch[i]) + get(os[i]); out.push(s.slice(0, 30));
P.x = 'changed'; out.push(get(ch[3]), get(os[3]));
var g = []; for (var k = 0; k < 6; k++) { var o = { ['g' + k]: 1 }; Object.defineProperty(o, 'x', { get: function () { return 'getter' + this.n; }, configurable: true }); o.n = k; g.push(o); }
s = []; for (var r = 0; r < 2; r++) for (var i = 0; i < 6; i++) s.push(get(g[i]), get(os[i])); out.push(s.slice(0, 6).join());
delete os[2].x; out.push(get(os[2]), get(os[1]));
os[4].x = 'set'; out.push(get(os[4]));
Object.defineProperty(os[6], 'x', { get() { return 'now a getter'; } }); out.push(get(os[6]));
var dict = {}; for (var k = 0; k < 200; k++) dict['d' + k] = k; dict.x = 'dict'; delete dict.d5; out.push(get(dict), get(dict));
var prox = new Proxy({ x: 'target' }, { get(t, k) { return 'trap ' + String(k); } }); out.push(get(prox));
out.push(get('str'), get(5), String(get([1])));
for (var round = 0; round < 30; round++) {
  var tmp = shapes(10, 'r' + round), t = '';
  for (var i = 0; i < 10; i++) t += get(tmp[i]);
  var junk = []; for (var j = 0; j < 3000; j++) junk.push({ j: j });
  if (round % 10 == 9) out.push(t.length);
}
var fresh = shapes(10, 'z'); t = ''; for (var i = 0; i < 10; i++) t += get(fresh[i]); out.push(t);
out.join(' ; ')
