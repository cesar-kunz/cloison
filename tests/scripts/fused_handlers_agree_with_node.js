// The interpreter's fused handlers and their fallbacks: `a.b.c` chains
// (`GetNamedStar…`) across a getter, a prototype's property, a prototype
// changed and a miss; postfix `++` and `--` kept as values (`PostInc…`)
// on int32s at their bounds and on what isn't one; a loop's `i++` before
// its test (`LdarIncStarLdar`), past 2^31; array literals of every element
// type, with holes and spreads (`PushLiteral`); `instanceof` on plain
// functions, bound ones, `Symbol.hasInstance`, proxies, primitives and a
// prototype that isn't an object, and after `prototype` is replaced. The
// expected value is Node's.
// expect: 2 1 3 4 TypeError 2 1 3 4 TypeError 2 1 3 4 TypeError 2 9 3 4 TypeError TypeError 0,1,2,3,2 2147483646,2147483647,2147483648,2147483649,2147483648 -2147483647,-2147483646,-2147483645,-2147483644,-2147483645 -2147483648,-2147483647,-2147483646,-2147483645,-2147483646 1.5,2.5,3.5,4.5,3.5 3,4,5,6,5 NaN,NaN,NaN,NaN,NaN 0,1,2,3,2 NaN,NaN,NaN,NaN,NaN 1,2,3,4,3 5n,6n,7n,8n,7n 1,2,3,4,3 10 75:9 0:0 2147483645 2147483646 2147483647 2147483648 2147483645 2147483646 2147483647 2147483648 [[1,2,3],[1,2.5,3],[1,"a",{}],[1,null,3],[1,2,3],[null,null],[[1],[2]]] 3,3,3,3,3,2,2 [[1,2,3],[1,2.5,3],[1,"a",{}],[1,null,3],[1,2,3],[null,null],[[1],[2]]] 3,3,3,3,3,2,2 [[1,2,3],[1,2.5,3],[1,"a",{}],[1,null,3],[1,2,3],[null,null],[[1],[2]]] 3,3,3,3,3,2,2 false 3 true true false false false false true true false true false true true true true true false false false false true true false true false true true true true true false false false false true true false true false true true true TypeError false true

var out = [];
function S(v) { return typeof v === "bigint" ? v + "n" : Object.is(v, -0) ? "-0" : String(v); }
function chain(o) { try { return o.a.b.c; } catch (e) { return e.constructor.name; } }
var shared = { b: { c: 1 } };
var objs = [{ a: { b: { c: 2 } } }, { a: Object.create(shared) }, { a: { get b() { return { c: 3 }; } } }, { a: { b: { c: 4, d: 1 } } }, { a: [1] }];
for (var r = 0; r < 3; r++) for (var i = 0; i < objs.length; i++) out.push(S(chain(objs[i])));
shared.b = { c: 9 };
for (var i = 0; i < objs.length; i++) out.push(S(chain(objs[i])));
out.push(chain({ a: null }));
function post(x, n) { var a = []; for (var k = 0; k < n; k++) a.push(x++); var b = x--; a.push(b, x); return a.map(S).join(","); }
var xs = [0, 2147483646, -2147483647, -2147483648, 1.5, "3", "x", null, undefined, true, 5n, { valueOf: function () { return 1; } }];
for (var i = 0; i < xs.length; i++) { try { out.push(post(xs[i], 3)); } catch (e) { out.push(e.constructor.name); } }
function sum(a) { var i = 0, s = 0; while (i < a.length) s += a[i++]; return s; }
out.push(S(sum([1, 2, 3, 4])));
function loops(n) { var c = 0, x = []; for (var i = 0; i < n; i++) { x[i] = c; c += i; i++; } for (var j = n; j > 0; j--) c += j; return c + ":" + x.length; }
out.push(loops(10), loops(0));
for (var r = 0; r < 2; r++) for (var i = 2147483645; i < 2147483649; i++) out.push(S(i));
function lits() { return [[1, 2, 3], [1, 2.5, 3], [1, "a", {}], [1, , 3], [...[1, 2], 3], [null, undefined], [[1], [2]]]; }
for (var r = 0; r < 3; r++) out.push(JSON.stringify(lits()), lits().map(function (x) { return x.length; }).join(","));
var hole = [1, , 3]; out.push(S(1 in hole), S(hole.length));
function A() {} function B() {} B.prototype = Object.create(A.prototype);
var bound = A.bind(null);
function H() {} Object.defineProperty(H, Symbol.hasInstance, { value: function (v) { return v === 1; } });
function NP() {} NP.prototype = 3;
var tests = [[new A(), A], [new B(), A], [new A(), B], [1, A], ["s", A], [null, A], [new A(), bound], [1, H], [2, H], [new Proxy(new B(), {}), A], [Object.create(null), A], [function () {}, Function], [[], Array], [[], Object]];
for (var r = 0; r < 3; r++) for (var i = 0; i < tests.length; i++) { try { out.push(S(tests[i][0] instanceof tests[i][1])); } catch (e) { out.push(e.constructor.name); } }
try { ({}) instanceof NP; } catch (e) { out.push(e.constructor.name); }
A.prototype = { x: 1 }; out.push(S(new B() instanceof A), S(new A() instanceof A));
out.join(" ");
