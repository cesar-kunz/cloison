// ArraySpeciesCreate is ArrayCreate without lookups for a plain array of
// the realm (`array::species_pristine`); a subclass, an own or inherited
// `constructor`, a null or replaced `@@species`, an accessor
// `Array.prototype.constructor` and a changed prototype still take the
// lookups, and getters are called as many times as before. Expected
// values from V8 and SpiderMonkey.
// expect: true ; true ; true ; true ; true ; true ; true ; {"0":2,"1":4,"n":2} ; true ; TypeError ; [2,3] ; 4 ; [1,2,3] ; 2 ; true ; 12 ; true ; false ; true

var out = [];
class MyA extends Array {}
var m = MyA.from([1, 2, 3]);
out.push(m.map(function (x) { return x; }) instanceof MyA, m.filter(Boolean) instanceof MyA, m.slice(1) instanceof MyA, m.splice(0, 1) instanceof MyA, [].concat.call(m, [4]) instanceof MyA, m.flat() instanceof MyA);
var a = [1, 2, 3]; a.constructor = function () {}; a.constructor[Symbol.species] = null; out.push(Array.isArray(a.map(function (x) { return x; })));
var b = [1, 2]; b.constructor = { [Symbol.species]: function (n) { this.n = n; } }; out.push(JSON.stringify(b.map(function (x) { return x * 2; })));
var c = [1]; c.constructor = undefined; out.push(Array.isArray(c.slice()));
try { var e = [1]; e.constructor = 5; e.slice(); } catch (x) { out.push(x.name); }
out.push(JSON.stringify([1, 2, 3].filter(function (x) { return x > 1; })), [1, 2].concat([3], 4).length, JSON.stringify([[1], [2, [3]]].flat(2)));
var calls = 0, sp = Object.getOwnPropertyDescriptor(Array, Symbol.species);
Object.defineProperty(Array, Symbol.species, { get: function () { calls++; return Array; }, configurable: true });
[1].map(function (x) { return x; }); [1].slice(); out.push(calls);
Object.defineProperty(Array, Symbol.species, sp);
var pc = Array.prototype.constructor; Array.prototype.constructor = MyA; out.push([1, 2].slice() instanceof MyA); Array.prototype.constructor = pc;
Object.defineProperty(Array.prototype, 'constructor', { get: function () { calls += 10; return Array; }, configurable: true }); [1].slice(); out.push(calls);
Object.defineProperty(Array.prototype, 'constructor', { value: pc, writable: true, configurable: true });
var proto = Object.create(Array.prototype), d = [1, 2]; Object.setPrototypeOf(d, proto); proto.constructor = MyA; out.push(d.map(function (x) { return x; }) instanceof MyA);
out.push([1, 2].map(function (x) { return x; }) instanceof MyA, Object.getPrototypeOf([3].filter(Boolean)) === Array.prototype);
out.join(' ; ')
