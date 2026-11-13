// The first strict `arguments` object stays the caller's: deleting its
// `length` and iterator and giving it many properties (its slots moved,
// a dictionary) leaves the next ones' `callee` the thrower pair. They
// took it from the first object's slot, read through the shape it had
// (a crash). Expected values from V8, JSC and QuickJS (SpiderMonkey
// lists `callee` before `length`, against 10.4.4.6's order).
// expect: function ; true ; false ; 2 ; 2,3 ; 0|1|length|callee|Symbol(Symbol.iterator) ; TypeError ; 51 ; false

'use strict';
var out = [];
function f() { return arguments; }
var a = f(1);
for (var i = 0; i < 50; i++) a['p' + i] = i;
delete a.p3; delete a.length; delete a[Symbol.iterator];
var b = f(2, 3), d = Object.getOwnPropertyDescriptor(b, 'callee');
out.push(typeof d.get, d.get === d.set, d.configurable, b.length, [...b].join(), Reflect.ownKeys(b).map(String).join('|'));
try { b.callee; } catch (x) { out.push(x.constructor.name); }
out.push(Reflect.ownKeys(a).length, 'length' in a);
out.join(' ; ')
