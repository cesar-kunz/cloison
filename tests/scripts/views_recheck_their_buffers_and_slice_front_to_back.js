// Views on a buffer check it again where the spec does: a `DataView`'s
// after its `byteLength`'s conversion, a view constructed with another
// `newTarget` after its `prototype`'s getter; and `slice` into a view of
// the same bytes copies front to back.
// expect: RangeError TypeError 1212

var r = [];
function t(f) { try { return f().byteLength; } catch (e) { return e.name; } }
var b = new ArrayBuffer(16, { maxByteLength: 32 });
r.push(t(() => new DataView(b, 0, { valueOf() { b.resize(4); return 8; } })));
var b3 = new ArrayBuffer(16); var nt = function () {}.bind();
Object.defineProperty(nt, "prototype", { get() { b3.transfer(); return Uint8Array.prototype; } });
r.push(t(() => Reflect.construct(Uint8Array, [b3, 0, 8], nt)));
var buf = new ArrayBuffer(8); new Uint8Array(buf).set([1, 2, 3, 4, 5, 6, 7, 8]);
class S extends Uint8Array { static get [Symbol.species]() { return function (n) { return new Uint8Array(buf, 2, n); }; } }
r.push(Array.from(new S(buf).slice(0, 4)).join(""));
r.join(" ");
