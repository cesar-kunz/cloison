// Deleting the property an object got last takes it back to the shape it
// had before, shared: bound functions (whose closure's `prototype` is
// deleted), objects adding then deleting, compiled reads after, and what
// may not (frozen, sealed, non-configurable, prototypes, accessors).
// expect: length,name | bound F | 1 | false | true | 1 | a |  | false | {"a":1,"b":3,"c":4} | {"y":2} | 50000 | false | 2 | false | 2 | true |  | k | 7 | true |  | a | false | 1 | 1,2,3 | 0,1,2 | 499500 | bound  | {"a":9}

var out = [];
function F(a, b) { this.v = a; }
var b = F.bind(null, 1);
out.push(Object.getOwnPropertyNames(b).join(), b.name, b.length, 'prototype' in b, new b() instanceof F, new b().v);
var o = {a: 1, b: 2}; delete o.b;
out.push(Object.keys(o).join(), o.b, 'b' in o);
o.b = 3; o.c = 4; out.push(JSON.stringify(o));
var p = {x: 1, y: 2}; delete p.x; out.push(JSON.stringify(p));
function get(r) { return r.y; }
var n = 0;
for (var i = 0; i < 100000; i++) { var r = {x: i, y: 2}; if (i & 1) delete r.y; if (get(r) === undefined) n++; }
out.push(n);
var fr = Object.freeze({a: 1, z: 2}); out.push(delete fr.z, fr.z);
var se = Object.seal({a: 1, z: 2}); out.push(delete se.z, se.z);
var pr = {k: 1, last: 2}; var child = Object.create(pr); child.k; out.push(delete pr.last, child.last, Object.keys(pr).join());
var ac = {a: 1}; Object.defineProperty(ac, 'g', {get() { return 7; }, configurable: true}); out.push(ac.g, delete ac.g, ac.g, Object.keys(ac).join());
var nc = {a: 1}; Object.defineProperty(nc, 'z', {value: 1, configurable: false}); out.push(delete nc.z, nc.z);
var arr = [1, 2]; arr.extra = 1; delete arr.extra; arr.push(3); out.push(arr.join(), Object.keys(arr).join());
var bs = []; for (var i = 0; i < 1000; i++) bs.push(function () { return this.q; }.bind({q: i}));
var t = 0; for (var i = 0; i < 1000; i++) t += bs[i](); out.push(t, bs[5].name);
var q = {a: 1, b: 2}; delete q.b; delete q.a; q.a = 9; out.push(JSON.stringify(q));
out.join(' | ')
