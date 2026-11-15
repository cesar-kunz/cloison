// Typed arrays' bulk paths against the element-by-element ones: `fill`,
// `reverse`, `indexOf`, `lastIndexOf` and `includes` on the bytes, a typed
// array made from or set from an array or another typed array, `Array.from`
// and spread of one, `Array.prototype.fill` on a plain array; with what
// goes around them: every kind's conversions, holes, values converted by
// code that changes the source or the buffer, shared, resized and detached
// buffers, iteration replaced and restored.
// The lines' count and a hash of them (`all`), as V8 and SpiderMonkey give
// them (JavaScriptCore but for three of its own).
// expect: 693:-2085273064
var out = [];
function show(v) {
  if (typeof v === 'bigint') return v + 'n';
  if (ArrayBuffer.isView(v)) return v.constructor.name + '[' + Array.prototype.map.call(v, show).join(',') + ']';
  if (Array.isArray(v)) { var s = []; for (var i = 0; i < v.length; i++) s.push(i in v ? show(v[i]) : '<hole>'); return '[' + s.join(',') + ']'; }
  if (typeof v === 'number' && v === 0 && 1 / v < 0) return '-0';
  return String(v);
}
function t(name, f) { var r; try { r = show(f()); } catch (e) { r = 'throws ' + e.constructor.name; } out.push(name + ': ' + r); }
var kinds = [Int8Array, Uint8Array, Uint8ClampedArray, Int16Array, Uint16Array, Int32Array, Uint32Array, Float32Array, Float64Array];
var src = [0, 1, -1, 2.5, 3.5, 255, 256, -129, 65536, 4294967296, NaN, -0, 1e40];
kinds.forEach(function (K) {
  var n = K.name;
  t(n + ' ctor array', function () { return new K(src); });
  t(n + ' from array', function () { return K.from(src); });
  t(n + ' set array', function () { var a = new K(15); a.set(src, 2); return a; });
  t(n + ' fill', function () { return new K(7).fill(300.5, 1, -1); });
  t(n + ' fill neg', function () { return new K(5).fill(-1.5, -3); });
  t(n + ' fill all', function () { return new K(9).fill(7); });
  t(n + ' reverse odd', function () { return new K([1, 2, 3, 4, 5]).reverse(); });
  t(n + ' reverse even', function () { return new K([1, 2, 3, 4, 5, 6]).reverse(); });
  t(n + ' reverse sub', function () { var a = new K([1, 2, 3, 4, 5, 6]); a.subarray(1, 5).reverse(); return a; });
  var a = new K(src);
  src.concat([300, 0.5, '1', null, undefined, 1n, true, -128, 127, 4294967295, 65535, -32768, 1.5, 2.5]).forEach(function (x) {
    t(n + ' indexOf ' + show(x), function () { return [a.indexOf(x), a.lastIndexOf(x), a.includes(x)]; });
  });
  t(n + ' indexOf from', function () { return [a.indexOf(1, 2), a.indexOf(1, -20), a.indexOf(0, Infinity), a.lastIndexOf(0, -1), a.lastIndexOf(1, 0), a.lastIndexOf(1, -Infinity), a.includes(1, 1), a.includes(1, 2)]; });
  kinds.forEach(function (L) {
    t(n + ' ctor ' + L.name, function () { return new K(new L(src)); });
    t(n + ' set ' + L.name, function () { var a = new K(15); a.set(new L(src), 1); return a; });
    t(n + ' set overlapping ' + L.name, function () { var b = new ArrayBuffer(64); var x = new L(b, 0, 6); x.set([1, 2, 3, 4, 5, 250]); var y = new K(b, 8, 6); y.set([9, 8, 7]); new K(b, 0, 6).set(x); y.set(new L(b, 0, 6)); return [new Uint8Array(b)]; });
  });
  t(n + ' Array.from', function () { return Array.from(new K(src)); });
  t(n + ' spread', function () { return [1, ...new K(src), 2]; });
  t(n + ' shared', function () { var a = new K(new SharedArrayBuffer(64)); a.set(src); a.fill(3, 1, 3).reverse(); return [a, a.indexOf(3), new K(a), Array.from(a)]; });
});
[BigInt64Array, BigUint64Array].forEach(function (K) {
  var n = K.name, bs = [0n, 1n, -1n, 2n ** 63n, 2n ** 64n + 5n];
  t(n + ' ctor', function () { return new K(bs); });
  t(n + ' from', function () { return K.from(bs); });
  t(n + ' ctor numbers', function () { return new K([1, 2]); });
  t(n + ' set numbers', function () { return new K(2).set([1, 2]); });
  t(n + ' set', function () { var a = new K(6); a.set(bs, 1); return a; });
  t(n + ' set mixed', function () { var a = new K(6); a.set(new Uint8Array(2)); return a; });
  t(n + ' ctor mixed', function () { return new K(new Uint8Array(2)); });
  t(n + ' ctor other', function () { return new K(new (K === BigInt64Array ? BigUint64Array : BigInt64Array)(bs)); });
  t(n + ' fill', function () { return new K(4).fill(-1n, 1, 3); });
  t(n + ' fill number', function () { return new K(4).fill(1); });
  t(n + ' reverse', function () { return new K(bs).reverse(); });
  t(n + ' search', function () { var a = new K(bs); return [a.indexOf(1n), a.indexOf(1), a.includes(-1n), a.lastIndexOf(0n), a.includes(18446744073709551615n), a.indexOf(5n)]; });
  t(n + ' Array.from', function () { return Array.from(new K(bs)); });
  t(n + ' spread', function () { return [...new K(bs)]; });
});
// Holes, other values, conversions that run code.
t('ctor holes', function () { return new Uint8Array([1, , 3]); });
t('ctor holes proto', function () { Array.prototype[1] = 9; try { return [new Uint8Array([1, , 3]), Uint8Array.from([1, , 3]), (function () { var a = new Uint8Array(3); a.set([1, , 3]); return a; })()]; } finally { delete Array.prototype[1]; } });
t('ctor strings', function () { return new Float64Array(['1.5', '0x10', '', 'x', null, undefined, true, [7], {}]); });
t('ctor valueOf mutates', function () { var a = [1, { valueOf: function () { a[2] = 50; a.length = 3; return 2; } }, 3, 4]; return [new Uint8Array(a), a]; });
t('from valueOf mutates', function () { var a = [1, { valueOf: function () { a[2] = 50; a.length = 3; return 2; } }, 3, 4]; return [Uint8Array.from(a), a]; });
t('set valueOf mutates', function () { var a = [1, { valueOf: function () { a[2] = 50; a.length = 3; return 2; } }, 3, 4]; var u = new Uint8Array(5).fill(9); u.set(a); return [u, a]; });
t('set valueOf to holes', function () { var a = [1, { valueOf: function () { a.length = 2; return 2; } }, 3, 4]; var u = new Uint8Array(5).fill(9); u.set(a); return [u, a]; });
t('set valueOf freezes', function () { var a = [1, { valueOf: function () { Object.defineProperty(a, 2, { get: function () { return 77; } }); return 2; } }, 3, 4]; var u = new Uint8Array(5).fill(9); u.set(a); return u; });
t('set valueOf shrinks', function () { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var u = new Uint8Array(b); var a = [1, { valueOf: function () { b.resize(3); return 2; } }, 3, 4, 5]; u.set(a); return [u, b.byteLength]; });
t('set valueOf detaches', function () { var b = new ArrayBuffer(8); var u = new Uint8Array(b); var a = [1, { valueOf: function () { b.transfer(); return 2; } }, 3, 4, 5]; u.set(a); return u.length; });
t('set array-like', function () { var u = new Uint8Array(4); u.set({ length: 3, 0: 5, 2: '7' }, 1); return u; });
t('set string', function () { var u = new Uint8Array(4); u.set('123', 1); return u; });
t('set too long', function () { new Uint8Array(2).set([1, 2, 3]); });
t('set offset', function () { new Uint8Array(4).set([1, 2, 3], 2); });
t('set neg offset', function () { new Uint8Array(4).set([1], -1); });
t('set getter array', function () { var a = [1, 2, 3]; Object.defineProperty(a, 1, { get: function () { return 42; } }); var u = new Uint8Array(3); u.set(a); return [u, new Uint8Array(a), Uint8Array.from(a)]; });
t('set frozen array', function () { var a = Object.freeze([1, 2, 3]); var u = new Uint8Array(3); u.set(a); return [u, new Uint8Array(a)]; });
t('set subclass array', function () { class A extends Array {} var a = A.from([1, 2, 3]); var u = new Uint8Array(3); u.set(a); return [u, new Uint8Array(a), Uint8Array.from(a)]; });
t('ctor array-like', function () { return new Uint8Array({ length: 3, 0: 5, 2: '7' }); });
t('from mapfn', function () { return Uint8Array.from([1, 2, 3], function (x, i) { return x * 2 + i; }); });
t('from typed mapfn', function () { return Uint8Array.from(new Float32Array([1.5, 2.5]), function (x) { return x * 2; }); });
t('from typed', function () { return Float32Array.from(new Uint8Array([1, 2, 3])); });
t('ctor resizable', function () { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var u = new Uint8Array(b); u.set([1, 2, 3]); b.resize(4); return [new Uint8Array(u), new Uint16Array(u), Array.from(u), [...u]]; });
t('ctor out of bounds', function () { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var u = new Uint8Array(b, 4, 4); b.resize(2); return new Uint8Array(u); });
t('Array.from out of bounds', function () { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var u = new Uint8Array(b, 4, 4); b.resize(2); return Array.from(u); });
t('spread detached', function () { var b = new ArrayBuffer(8); var u = new Uint8Array(b); b.transfer(); return [...u]; });
t('fill detached', function () { var b = new ArrayBuffer(8); var u = new Uint8Array(b); b.transfer(); return u.fill(1); });
t('fill value detaches', function () { var b = new ArrayBuffer(8); var u = new Uint8Array(b); return u.fill({ valueOf: function () { b.transfer(); return 1; } }); });
t('fill start shrinks', function () { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var u = new Uint8Array(b); u.fill(1, { valueOf: function () { b.resize(4); return 1; } }); return u; });
t('fill ranges', function () { return [new Uint8Array(4).fill(1, 3, 1), new Uint8Array(4).fill(1, 9), new Uint8Array(4).fill(1, -9, 9), new Uint8Array(4).fill(1, NaN, 2), new Uint8Array(0).fill(1), new Int16Array(3).fill(-2, 2)]; });
t('includes shrinks', function () { var b = new ArrayBuffer(8, { maxByteLength: 16 }); var u = new Uint8Array(b); var from = { valueOf: function () { b.resize(4); return 0; } }; return [u.includes(undefined, from), u.includes(0, 5), u.indexOf(0, 5), u.lastIndexOf(0, 7), u.indexOf(undefined)]; });
t('indexOf grows', function () { var b = new ArrayBuffer(4, { maxByteLength: 16 }); var u = new Uint8Array(b); return u.indexOf(7, { valueOf: function () { b.resize(8); u[6] = 7; return 0; } }); });
t('f32 search', function () { var a = new Float32Array([0.1, -0, NaN, 1.5, Infinity]); return [a.indexOf(0.1), a.indexOf(Math.fround(0.1)), a.indexOf(0), a.includes(NaN), a.indexOf(NaN), a.lastIndexOf(1.5), a.indexOf(Infinity), a.includes(-0)]; });
t('f16 search', function () { if (typeof Float16Array === 'undefined') return 'none'; var a = new Float16Array([0.5, -0, NaN, 1.5]); return [a.indexOf(0.5), a.includes(NaN), a.indexOf(0), a.lastIndexOf(1.5), new Float16Array(3).fill(1.5), a.reverse().length, [...a].length]; });
// Iteration changed.
var u = new Uint8Array([1, 2, 3]);
function gen() { return [7, 8][Symbol.iterator](); }
t('own iterator', function () { var v = new Uint8Array([1, 2, 3]); Object.defineProperty(v, Symbol.iterator, { value: gen }); return [Array.from(v), [...v], Uint16Array.from(v), new Uint16Array(v)]; });
t('kind proto iterator', function () { Uint8Array.prototype[Symbol.iterator] = gen; try { return [Array.from(u), [...u], Uint16Array.from(u), new Uint16Array(u), Array.from(new Int8Array(2))]; } finally { delete Uint8Array.prototype[Symbol.iterator]; } });
t('restored', function () { return [Array.from(u), [...u], Uint16Array.from(u)]; });
var TA = Object.getPrototypeOf(Uint8Array.prototype), orig = TA[Symbol.iterator];
t('TA iterator', function () { TA[Symbol.iterator] = gen; try { return [Array.from(u), [...u], Uint16Array.from(u), new Uint16Array(u)]; } finally { TA[Symbol.iterator] = orig; } });
t('restored 2', function () { return [Array.from(u), [...u], Uint16Array.from(u)]; });
t('TA iterator deleted', function () { delete TA[Symbol.iterator]; try { return [Array.from(u), Uint16Array.from(u)]; } finally { Object.defineProperty(TA, Symbol.iterator, { value: orig, writable: true, configurable: true }); } });
t('TA iterator deleted spread', function () { delete TA[Symbol.iterator]; try { return [...u]; } finally { Object.defineProperty(TA, Symbol.iterator, { value: orig, writable: true, configurable: true }); } });
// The method read, then gone: the one read is the one called.
function selfDeleting(v) { Object.defineProperty(v, Symbol.iterator, { configurable: true, get: function () { delete v[Symbol.iterator]; return function* () { yield 9; }; } }); return v; }
t('getter deletes itself', function () { return [new Uint8Array(selfDeleting([1, 2])), Uint8Array.from(selfDeleting([1, 2])), new Uint8Array(selfDeleting(new Uint8Array([1, 2]))), Uint8Array.from(selfDeleting(new Int8Array([1, 2])))]; });
function arm(P, o) { Object.defineProperty(P, Symbol.iterator, { configurable: true, get: function () { Object.defineProperty(P, Symbol.iterator, { value: o, writable: true, configurable: true }); return function* () { yield 9; }; } }); }
t('getter restores the builtin', function () { var r = []; arm(Array.prototype, Array.prototype[Symbol.iterator]); r.push(new Uint8Array([1, 2])); arm(Array.prototype, Array.prototype[Symbol.iterator]); r.push(Uint8Array.from([1, 2])); arm(TA, orig); r.push(Uint8Array.from(new Int8Array([1, 2]))); return r; });
var AIP = Object.getPrototypeOf([][Symbol.iterator]()), next = AIP.next;
t('next changed', function () { var n = 0; AIP.next = function () { return n++ < 2 ? { value: 40 + n, done: false } : { done: true }; }; try { return [Array.from(u)]; } finally { AIP.next = next; } });
t('next changed ctor', function () { var n = 0; AIP.next = function () { return n++ < 2 ? { value: 40 + n, done: false } : { done: true }; }; try { return [new Uint8Array([5, 6, 7])]; } finally { AIP.next = next; } });
t('next changed from', function () { var n = 0; AIP.next = function () { return n++ < 2 ? { value: 40 + n, done: false } : { done: true }; }; try { return [Uint8Array.from(u)]; } finally { AIP.next = next; } });
t('array iterator changed', function () { var o = Array.prototype[Symbol.iterator]; Array.prototype[Symbol.iterator] = function () { var n = 0; return { next: function () { return n++ < 2 ? { value: 60 + n, done: false } : { done: true }; } }; }; try { return [new Uint8Array([5, 6, 7]), Uint8Array.from([5, 6, 7])]; } finally { Array.prototype[Symbol.iterator] = o; } });
t('restored 3', function () { return [Array.from(u), [...u], Uint16Array.from(u), new Uint8Array([5, 6, 7])]; });
t('subclass', function () { class U extends Uint8Array { *[Symbol.iterator]() { yield 9; } } var v = new U([1, 2]); return [Array.from(v), [...v], Uint8Array.from(v), new Uint8Array(v)]; });
t('setPrototypeOf', function () { var v = new Uint8Array([1, 2]); Object.setPrototypeOf(v, { [Symbol.iterator]: gen, __proto__: Uint8Array.prototype }); return [Array.from(v), [...v]]; });
t('Array.from mapfn', function () { return Array.from(u, function (x, i) { return x * 10 + i; }); });
t('Array.from big', function () { var v = new Uint16Array(70000); v[69999] = 5; var a = Array.from(v); return [a.length, a[69999], a[0]]; });
// Array.prototype.fill.
t('array fill', function () { return [[1, 2, 3, 4].fill(0, 1, 3), [1, 2, 3].fill('x'), new Array(4).fill(1.5), new Array(4).fill(), [1, , 3].fill({}, 1).length, [].fill(1), [1, 2, 3].fill(9, -1), [1, 2, 3].fill(9, 2, 1), [1.5, 2.5].fill(1), [1, 2].fill(null), ['a', 'b'].fill(7, 1)]; });
t('array fill holes', function () { var a = [1, , 3, , 5]; a.fill(7, 1, 4); return a; });
t('array fill same object', function () { var o = {}; var a = new Array(3).fill(o); return a[0] === o && a[2] === o; });
t('array fill frozen', function () { return Object.freeze([1, 2]).fill(3); });
t('array fill sealed', function () { return Object.seal([1, 2]).fill(3); });
t('array fill setter', function () { var hit = []; Object.defineProperty(Array.prototype, 1, { set: function (v) { hit.push(v); }, configurable: true }); try { var a = [1, , 3]; a.fill(7); return [a, hit]; } finally { delete Array.prototype[1]; } });
t('array fill array-like', function () { return Array.prototype.fill.call({ length: 3, 1: 5 }, 8)[2]; });
t('array fill valueOf', function () { var a = [1, 2, 3, 4]; a.fill(0, { valueOf: function () { a.length = 2; return 1; } }); return a; });
t('array fill typed', function () { return Array.prototype.fill.call(new Uint8Array(3), 300); });
t('array fill then push', function () { var a = new Array(3).fill(1); a.push('s'); a.fill(2.5, 0, 2); return a; });
var h = 0, all = out.join('\n');
for (var i = 0; i < all.length; i++) h = (h * 31 + all.charCodeAt(i)) | 0;
out.length + ':' + h;
