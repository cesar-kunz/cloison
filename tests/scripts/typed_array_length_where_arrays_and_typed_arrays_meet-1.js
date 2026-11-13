// A site that met arrays first takes a typed array's `length` through
// the runtime, shadowed or not; one typed by a field (`Ty::Ext`)
// drops the check of the shape until a typed array gets an own
// `length`.
// The other getters on %TypedArray%.prototype are not `length`'s.
// expect: 2280000 x3

function len(o) { return o.length; }
var arr = [1, 2, 3], ta = new Uint8Array(4), s = 0;
for (var i = 0; i < 20000; i++) s += len(arr);
for (var i = 0; i < 20000; i++) s += len(ta) + len(arr);
var sh = new Uint8Array(2); Object.defineProperty(sh, 'length', {value: 100});
for (var i = 0; i < 20000; i++) s += len(sh) + len(ta);
var sh2 = new Int8Array(2); Object.defineProperty(sh2, 'length', {value: 'x'});
var t = '';
for (var i = 0; i < 2000; i++) t = len(sh2) + len(arr);
s + ' ' + t;
