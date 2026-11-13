// A site that met arrays first takes a typed array's `length` through
// the runtime, shadowed or not; one typed by a field (`Ty::Ext`)
// drops the check of the shape until a typed array gets an own
// `length`.
// The other getters on %TypedArray%.prototype are not `length`'s.
// expect: 300,100,500

var holder = {t: new Uint8Array(3)};
function f(h) { var s = 0; for (var i = 0; i < 100; i++) s += h.t.length; return s; }
var r = [];
for (var k = 0; k < 300; k++) r[0] = f(holder);
var o = new Uint8Array(3); Object.defineProperty(o, 'length', {value: 1}); holder.t = o;
r[1] = f(holder);
holder.t = new Uint8Array(5);
for (var k = 0; k < 300; k++) r[2] = f(holder);
r.join();
