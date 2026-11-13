// A collection subclass's constructor gives its object the subclass's
// prototype before looking its adder up: an overridden `set` or `add` is
// called for each item, and `new.target`'s `prototype` read first.
// expect: true,2,1 | true,2,3 | true,1,1 | true,true,1 | adder threw | NT.set | true,4 | proto,iter | true | 1

var out = [];
var calls = 0;
class M extends Map { set(k, v) { calls++; return super.set(k, v); } }
var m = new M([[1, 2]]); out.push([m instanceof M, m.get(1), calls].join(','));
var sc = 0; class S extends Set { add(v) { sc++; return super.add(v); } }
var s = new S([1, 2, 2]); out.push([s instanceof S, s.size, sc].join(','));
var wc = 0; class WM extends WeakMap { set(k, v) { wc++; return super.set(k, v); } }
var k = {}; var wm = new WM([[k, 1]]); out.push([wm instanceof WM, wm.get(k), wc].join(','));
var wsc = 0; class WS extends WeakSet { add(v) { wsc++; return super.add(v); } }
var ws = new WS([k]); out.push([ws instanceof WS, ws.has(k), wsc].join(','));
class T extends Map { set() { throw 'adder threw'; } }
try { new T([[1, 1]]); out.push('no throw'); } catch (e) { out.push(e); }
function NT() {} NT.prototype = Object.create(Map.prototype, { set: { value: function (k, v) { out.push('NT.set'); return Map.prototype.set.call(this, k, v); } } });
var r = Reflect.construct(Map, [[[3, 4]]], NT); out.push([Object.getPrototypeOf(r) === NT.prototype, r.get(3)].join(','));
var log = []; var nt = function () {}.bind(); Object.defineProperty(nt, 'prototype', { get() { log.push('proto'); return Set.prototype; } });
Reflect.construct(Set, [{ [Symbol.iterator]() { log.push('iter'); return [][Symbol.iterator](); } }], nt); out.push(log.join(','));
class N extends Map {} out.push(new N().constructor === N, new Map([[1,1]]).size);
out.join(' | ');
