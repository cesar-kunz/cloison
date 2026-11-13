// Array methods over a far array visit its present elements without
// stepping through the missing indices (lengths near 2^32 finish at
// once), elements added by a callback included; `includes(undefined)`
// sees the holes; a join longer than a string can be throws at once.
// (SpiderMonkey, JSC and V8 agree with every result at lengths near 4e6.)
// expect: 0:1,1:2,2:3,1000000000:added,2000000000:x,3000000000:y,4000000000:z | 3000000000 | 1 | 4000000000 | true | false | true | 4000000001 | 0,1,2,2000000000,3000000000,4000000000 | x,y,z | 123xyz | zyx321 | 6xyz | true | true | 2999999999 | 0,1999999998,2999999998 | 4000000002 | 0,1,2,3,2000000001,3000000001,4000000001 | 4000000001 | 0,1,2,3,4,5 | 1,2,3,x,y,z | RangeError | 0,1,2

function mk() { var a = [1, 2, 3]; a[4e9] = 'z'; a[3e9] = 'y'; a[2e9] = 'x'; return a; }
var out = [];
var a = mk(), seen = [];
a.forEach(function (v, i) { seen.push(i + ':' + v); if (i == 2) a[1e9] = 'added'; });
out.push(seen.join());
out.push(mk().indexOf('y'), mk().lastIndexOf(2), mk().lastIndexOf('z'), mk().includes(undefined), [1, 2].includes(undefined), mk().includes('x'));
out.push(mk().map(function (v) { return v + '!'; }).length, Object.keys(mk().map(function (v) { return v + '!'; })).join());
out.push(mk().filter(function (v) { return typeof v == 'string'; }).join());
out.push(mk().reduce(function (s, v) { return s + v; }, ''), mk().reduceRight(function (s, v) { return s + v; }, ''), mk().reduce(function (s, v) { return s + v; }));
out.push(mk().some(function (v) { return v == 'z'; }), mk().every(function (v) { return v; }));
var s = mk().slice(2, 3e9 + 1); out.push(s.length, Object.keys(s).join());
var c = [0].concat(mk()); out.push(c.length, Object.keys(c).join());
var so = mk(); so.sort(); out.push(so.length, Object.keys(so).join(), so.slice(0, 6).join());
try { mk().join(); } catch (e) { out.push(e.constructor.name); }
var f = []; f[1e7] = [1, [2]]; f[5] = 0; out.push(f.flat(2).join());
out.join(' | ');
