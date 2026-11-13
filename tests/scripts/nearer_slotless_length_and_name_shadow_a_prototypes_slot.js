// OrdinaryGet stops at the first object with an own property: an
// array's `length`, a function's `length` and `name` have no slot
// but shadow a farther prototype's.
// expect: foo0,foo0,3,3,2

var fn = function foo() {};
Object.setPrototypeOf(fn, {name: 'x', length: 9});
var o = Object.create(fn), r = [];
for (var i = 0; i < 3000; i++) r[i % 2] = o.name + o.length;
var arr = [1, 2, 3]; Object.setPrototypeOf(arr, {length: 99});
var q = Object.create(arr);
for (var i = 0; i < 3000; i++) r[2] = q.length;
r[3] = Reflect.get(arr, 'length', 5);
function C() { this.n = this.length; }
var cp = [1, 2]; Object.setPrototypeOf(cp, {length: 99}); C.prototype = cp;
for (var i = 0; i < 3000; i++) r[4] = new C().n;
r.join();
