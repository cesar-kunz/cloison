// Strict functions' `arguments` objects after the first get its shape
// (transitions of the same keys and attributes) and its `callee` pair
// (`Vm::strict_arguments`): own keys in order, the descriptors of
// `length`, `callee` (one thrower as getter and setter, not
// configurable), the elements and `@@iterator`; changing, freezing and
// deleting one leaves the others as they were; they are unmapped.
// Expected values from V8 and SpiderMonkey.
// expect: 0 ; 3 ; x ; 1 ; true ; 4 ; 0|1|2|length|callee|Symbol(Symbol.iterator) ; 0,1,2 ; true/false/true ; acc:false/false/true ; true/true/true ; true/false/true ; true ; [object Arguments] ; 3 ; false ; 1 ; TypeError ; set TypeError ; def TypeError ; 7 ; 1,2,9 ; 0 ; 1 ; 1 ; 0:w ; 2:w ; false ; true ; {"0":1,"1":[2]} ; 3

'use strict';
var out = [];
function d(o, k) { var x = Object.getOwnPropertyDescriptor(o, k); return x ? (x.get ? 'acc:' + x.enumerable + '/' + x.configurable + '/' + (x.get === x.set) : [x.writable, x.enumerable, x.configurable].join('/')) : 'none'; }
function f() { return arguments; }
var a = f(), b = f(1, 'x', { o: 1 }), c = f(undefined), e = f(1.5, 2.5);
out.push(a.length, b.length, b[1], c.length, 0 in c, e[0] + e[1]);
out.push(Reflect.ownKeys(b).map(String).join('|'), Object.keys(b).join(), d(b, 'length'), d(b, 'callee'), d(b, 0), d(b, Symbol.iterator));
out.push(Object.getOwnPropertyDescriptor(a, 'callee').get === Object.getOwnPropertyDescriptor(b, 'callee').get, Object.prototype.toString.call(b), [...b].length, Array.isArray(b));
a[Symbol.iterator] = null; out.push([...f(1)].join());
try { b.callee; } catch (x) { out.push(x.constructor.name); }
try { b.callee = 1; } catch (x) { out.push('set ' + x.constructor.name); }
try { Object.defineProperty(b, 'callee', { value: 1 }); } catch (x) { out.push('def ' + x.constructor.name); }
b.length = 7; b[9] = 'n'; delete b[0]; out.push(b.length, Object.keys(b).join(), a.length, f(4).length);
function g(x) { x = 2; return arguments[0]; } out.push(g(1));
function h() { arguments[0] = 'w'; return arguments.length + ':' + arguments[0]; } out.push(h(), h(1, 2));
var o = f(1, 2); Object.freeze(o); out.push(Object.isFrozen(f(1)), Object.isFrozen(o));
out.push(JSON.stringify(f(1, [2])), Reflect.ownKeys(f()).length);
out.join(' ; ')
