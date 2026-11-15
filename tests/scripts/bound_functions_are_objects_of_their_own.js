// expect: ["length","name"]|{"value":2,"writable":false,"enumerable":false,"configurable":true}|{"value":"bound f","writable":false,"enumerable":false,"configurable":true}|1,p,q,r,4|1,p,q,,2|3 0|1,1,2,3,11|false|function () { [native code] }|bound bound f 1 1,p,q,r,3|6 bound renamed|null|0|0 [bound ]|0 [bound ]|bound get w|bound |bound |bound max|1|bound [d]|true|true|[1,2,true,"P",true,true,true]|[1,2,true,true]|true|[1,5,1,true,true,true]|[1,9,true,true]|[[1,2,3],[1,2,4],true]|[[1,2,9],true,true]|5 string|true|true|true|true|true|[1,2,"1,p,q,r,4",["extra","other"]]|1,p,z,,2|true|thrown|length,name 2 bound acc|0,a0,0,z,3|2500,a2500,2500,z,3|5000,a5000,5000,z,3|7500,a7500,7500,z,3|215|1,m,1,0,4;1,m,2,1,4;1,m,3,2,4|1,1,2,,2 1,1,,,1|[object Function] function
// A bound function: its own properties, prototype, calls and constructions.
var out = [];
function show(v) { out.push(typeof v === 'string' ? v : JSON.stringify(v) === undefined ? String(v) : JSON.stringify(v)); }
function f(a, b, c) { return [this && this.x, a, b, c, arguments.length].join(); }
var o = { x: 1 };
var g = f.bind(o, 'p');
show(Object.getOwnPropertyNames(g));
show(Object.getOwnPropertyDescriptor(g, 'length'));
show(Object.getOwnPropertyDescriptor(g, 'name'));
show(g('q', 'r', 's'));
show(g.call({ x: 2 }, 'q'));
show(f.bind().length + ' ' + f.bind(o, 1, 2, 3, 4).length);
show(f.bind(o, 1, 2, 3, 4, 5, 6, 7, 8, 9)(10, 11));
show('prototype' in g);
show(String(g).replace(/\s+/g, ' '));
// Bound again: the first `this` stays, the arguments add up.
var gg = g.bind({ x: 3 }, 'q');
show(gg.name + ' ' + gg.length + ' ' + gg('r'));
// The target's own length and name, read at the bind.
function h() {}
Object.defineProperty(h, 'length', { value: 7.9 });
Object.defineProperty(h, 'name', { value: 'renamed' });
show(h.bind(null, 1).length + ' ' + h.bind().name);
Object.defineProperty(h, 'length', { value: Infinity });
show(h.bind(null, 1).length);
Object.defineProperty(h, 'length', { value: -Infinity });
show(h.bind(null, 1).length);
Object.defineProperty(h, 'length', { value: 'x' });
Object.defineProperty(h, 'name', { value: 5 });
show(h.bind().length + ' [' + h.bind().name + ']');
function k(a, b) {}
delete k.length; delete k.name;
show(k.bind().length + ' [' + k.bind().name + ']');
var getter = Object.getOwnPropertyDescriptor({ get w() { return 1; } }, 'w').get;
show(getter.bind().name);
show((function () {}).bind().name + '|' + (() => 1).bind().name + '|' + Math.max.bind().name + '|' + Math.max.bind(null, 1).length);
var sym = Symbol('d'); var os = { [sym]() {} };
show(os[sym].bind().name);
// Its prototype is the target's.
class A extends Array {}
var BA = A.bind(null);
show(Object.getPrototypeOf(BA) === Array);
var pf = function () {}; Object.setPrototypeOf(pf, null);
show(Object.getPrototypeOf(Function.prototype.bind.call(pf)) === null);
// Constructing: `new.target` is the target; `this` is ignored.
function P(a, b) { this.a = a; this.b = b; this.nt = new.target === P; }
P.prototype.tag = 'P';
var BP = P.bind({ ignored: 1 }, 1);
var p = new BP(2);
show([p.a, p.b, p.nt, p.tag, p instanceof P, p instanceof BP, Object.getPrototypeOf(p) === P.prototype]);
class C { constructor(a, b) { this.a = a; this.b = b; this.nt = new.target; } }
var BC = C.bind(null, 1), BBC = BC.bind(null, 2);
var c = new BBC(3);
show([c.a, c.b, c.nt === C, c instanceof BBC]);
try { class X extends BC {} show('extended'); } catch (e) { show(e instanceof TypeError); }
BC.prototype = C.prototype;
class D extends BC { constructor() { super(5); this.d = 1; } }
var d = new D();
show([d.a, d.b, d.d, d.nt === D, d instanceof D, d instanceof C]);
var r = Reflect.construct(BC, [9], D);
show([r.a, r.b, r.nt === D, Object.getPrototypeOf(r) === D.prototype]);
var BArr = Array.bind(null, 1, 2);
show([new BArr(3), BArr(4), new BArr(3) instanceof Array]);
BArr.prototype = Array.prototype;
class E2 extends BArr {}
show([new E2(9), new E2() instanceof E2, Array.isArray(new E2())]);
var BD = Date.bind(null, 2020, 0);
show(new BD(5).getDate() + ' ' + typeof BD());
// Not constructors: arrows, methods, bound ones of those.
for (var nc of [(() => 1).bind(), ({ m() {} }).m.bind(), Math.max.bind()]) {
  try { new nc(); show('constructed'); } catch (e) { show(e instanceof TypeError); }
}
try { class F extends (() => 1).bind() {} show('extended'); } catch (e) { show(e instanceof TypeError); }
try { Function.prototype.bind.call({}); } catch (e) { show(e instanceof TypeError); }
// Properties of its own, extensibility, a throwing target.
g.extra = 1; g.other = 2;
show([g.extra, g.other, g('q', 'r', 's'), Object.keys(g)]);
Object.freeze(g);
show(g('z'));
show(delete f.bind().name);
var t = function () { throw new RangeError('thrown'); }.bind();
try { t(); } catch (e) { show(e.message); }
// Getters run at the bind, in order: length, then name.
var log = [];
var acc = function () {};
Object.defineProperty(acc, 'length', { get() { log.push('length'); return 3; } });
Object.defineProperty(acc, 'name', { get() { log.push('name'); return 'acc'; } });
var ba = acc.bind(null, 1);
show(log.join() + ' ' + ba.length + ' ' + ba.name);
// Many, with collections between: the hidden slots are traced.
var keep = [];
for (var i = 0; i < 20000; i++) { keep.push(f.bind({ x: i }, 'a' + i, [i])); if (i % 5000 == 0) show(keep[i >> 1]('z')); }
var sum = 0; for (var i = 0; i < keep.length; i += 1999) sum += keep[i]('z').length;
show(sum);
// apply, call, callbacks, toString's tag.
show([1, 2, 3].map(f.bind(o, 'm')).join(';'));
show(f.bind(o).apply(null, [1, 2]) + ' ' + Function.prototype.call.bind(f)(o, 1));
show(Object.prototype.toString.call(g) + ' ' + typeof g);
out.join('|');
