// A site that met arrays first takes a typed array's `length` through
// the runtime, shadowed or not; one typed by a field (`Ty::Ext`)
// drops the check of the shape until a typed array gets an own
// `length`.
// The other getters on %TypedArray%.prototype are not `length`'s.
// expect: 3,3,3,0

var u8 = new Uint8Array(3), r = [];
function f(t) { return [t.length, t.buffer.byteLength, t.byteLength, t.byteOffset]; }
for (var i = 0; i < 3000; i++) r = f(u8);
r.join();
