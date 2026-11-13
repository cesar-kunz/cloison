// Prototypes getting properties after their objects were read (each a
// unique shape, made from the one before, which is no longer kept as its
// parent): `Object.create` chains, a constructor's prototype getting a
// method and losing it, many properties then elements and a delete, class
// methods added late, an array made a prototype.
// expect: 708000 | 20000 | n | false | 44847 | e | false | 300 | 44999 | function | 5 | 1.5 | x

var out = [];
// Attribute lists inheriting through Object.create chains, each one a prototype that gets properties after.
var base = {a: 1};
var chain = [base];
for (var i = 1; i < 200; i++) { var o = Object.create(chain[i - 1]); o['k' + i] = i; o.a = i; chain.push(o); }
function rd(o, k) { return o[k]; }
var s = 0;
for (var r = 0; r < 2000; r++) s += rd(chain[199], 'a') + rd(chain[199], 'k5') + (rd(chain[199], 'k150') | 0);
out.push(s);
// A prototype getting properties after its objects were read, compiled.
function P() {} var p = P.prototype;
var objs = []; for (var i = 0; i < 100; i++) objs.push(new P());
function get(o) { return o.m === undefined ? 0 : o.m(); }
var t = 0;
for (var i = 0; i < 50000; i++) { t += get(objs[i % 100]); if (i === 20000) p.m = function () { return 1; }; if (i === 30000) p.n = 2; if (i === 40000) delete p.m; }
out.push(t, Object.keys(p).join(), 'm' in objs[0]);
// Many properties added to one prototype, then elements and a delete.
var q = {}; var c = Object.create(q);
for (var i = 0; i < 300; i++) q['p' + i] = i;
q[0] = 'e'; delete q.p3;
var u = 0; for (var i = 0; i < 300; i++) u += c['p' + i] | 0;
out.push(u, c[0], 'p3' in c, Object.keys(q).length);
// A class hierarchy with methods added late.
class A { f() { return 1; } } class B extends A {} var b = new B();
var w = 0; for (var i = 0; i < 30000; i++) { w += b.f(); if (i === 10000) A.prototype.g = () => 5; if (i === 15000) B.prototype.f = function () { return 2; }; }
out.push(w, typeof b.g);
// An object made a prototype, then its elements' type changing.
var e = [1, 2, 3]; var ec = Object.create(e); e.push(1.5); e.push('x');
out.push(ec.length, ec[3], ec[4]);
out.join(' | ')
