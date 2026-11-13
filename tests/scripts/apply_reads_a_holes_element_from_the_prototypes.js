// (cloison-fuzz triage/BUGS.md #29: the fast path of `f.apply(that,
// list)` passed a hole of a plain array as `undefined`, where
// CreateListFromArrayLike's Get finds a prototype's element. V8, JSC,
// SpiderMonkey and QuickJS agree.)
// expect: 1,,3,3 | 1,inherited,3,3 | 1,inherited,3,3 | 1,2,3,3 | ,inherited,7 | 1,inherited,getter,3 | ,inherited,getter,3 | 1,,3,3 | 200000000 | 2:5,6

var r = [];
function f(u, v, w) { return [u, v, w, arguments.length].join(","); }
r.push(f.apply(null, [1, , 3]));
Array.prototype[1] = "inherited";
r.push(f.apply(null, [1, , 3]), Reflect.apply(f, null, [1, , 3]), f.apply(null, [1, 2, 3]));
function g() { return Array.prototype.slice.call(arguments).join(","); }
r.push(g.apply(null, [, , 7]));
Object.defineProperty(Object.prototype, "2", { get() { return "getter"; }, configurable: true });
r.push(f.apply(null, [1, , ,]), f.apply(null, new Array(3)));
delete Object.prototype[2]; delete Array.prototype[1];
r.push(f.apply(null, [1, , 3]));
var s = 0; function h(a, b) { return a + (b === undefined ? 0 : b); }
for (var i = 0; i < 20000; i++) s += h.apply(null, i & 1 ? [i, , ] : [i, 1]);
r.push(s);
function mut(a, b) { lst.length = 0; return arguments.length + ":" + a + "," + b; } var lst = [5, 6];
r.push(mut.apply(null, lst));
r.join(" | ");
