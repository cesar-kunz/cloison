// An object literal defines its properties: a prototype's setter or
// read-only property of the name doesn't apply, also once compiled code
// caches the literal's stores (as an assignment's does apply).
// expect: 4999,a|b|c,pa,0,2,a|b|c,1

var log = [], r = [];
function mk(x) { return { a: x, b: 2, c: 3 }; }
function plain(o, x) { o.a = x; return o; }
var o;
for (var i = 0; i < 5000; i++) { o = mk(i); plain({}, i); }
Object.defineProperty(Object.prototype, "a", { set: function (v) { log[log.length] = v; }, get: function () { return "pa"; }, configurable: true });
for (var i = 0; i < 5000; i++) { o = mk(i); }
r.push(o.a, Object.keys(o).join("|"));
var p = plain({}, 7); r.push(p.a, Object.keys(p).length);
Object.defineProperty(Object.prototype, "b", { value: 0, writable: false, configurable: true });
for (var i = 0; i < 3000; i++) { o = mk(i); }
r.push(o.b, Object.keys(o).join("|"), log.length);
delete Object.prototype.a; delete Object.prototype.b;
r.join();
