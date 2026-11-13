// Arguments a native or a function gets from a Rust list (`apply`'s,
// `Reflect`'s, a bound function's) are read from the VM stack, where they
// are copied before the call: natives reading them again after calling
// into JS, `arguments` and rest made after entry, construction with a
// `new.target` whose `prototype` getter runs first, 100,000 of them, bound
// functions of bound functions.
// expect: 2,4,6 | 5,6 | 1,2,3,5 | 7,8,2,4 | 1,2,1,3 | 13 | 6 | 2 | true | xy | false | true | 99999 | 100000 | hi | 3 | p,q,1,3

var out = [];
function churn() { var j = []; for (var i = 0; i < 3000; i++) j.push({i}); return j.length; }
out.push(Array.prototype.map.apply([1, 2, 3], [function (x) { churn(); return x * 2; }]).join());
var bmap = Array.prototype.map.bind([4, 5]); out.push(bmap(function (x) { churn(); return x + 1; }).join());
function f(a, b, ...r) { churn(); return [a, b, r.length, arguments.length].join(); }
var bf = f.bind(null, 1, 2); out.push(bf(3, 4, 5));
out.push(f.apply(null, [7, 8, 9, 10]), Reflect.apply(f, null, [1, 2, 3]));
function C(a, ...r) { churn(); this.s = a + r.length; }
var BC = C.bind(null, 10); out.push(new BC(1, 2, 3).s, Reflect.construct(C, [5, 6]).s);
class K { constructor(...a) { this.n = a.length; } } class D extends K {}
out.push(Reflect.construct(K, [1, 2], D).n, Reflect.construct(K, [1, 2], D) instanceof D);
var NT = new Proxy(function () {}, {get(t, k) { churn(); return k === 'prototype' ? Array.prototype : t[k]; }});
var made = Reflect.construct(function (a, b) { this.v = Object.keys(a)[0] + Object.keys(b)[0]; }, [{x: 1}, {y: 2}], NT);
out.push(made.v, Array.isArray(made), made instanceof Array);
var big = []; for (var i = 0; i < 100000; i++) big.push(i);
out.push(Math.max.apply(null, big), (function () { return arguments.length; }).apply(null, big));
out.push(String.fromCharCode.apply(null, [104, 105]), [].concat.apply([], [[1], [2, [3]]]).length);
var bb = f.bind(null, 'p').bind(null, 'q'); out.push(bb('r'));
out.join(' | ')
