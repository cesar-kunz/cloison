// Issue #272: %TypedArray%.prototype.length is an accessor; compiled
// code reads the length's field only for typed arrays whose shapes
// were checked to reach the builtin getter under the prototype epoch
// and while its slot holds it.
// expect: ok

function eq(a, b, l) { if (a !== b) throw new Error(l + ': ' + a + ' !== ' + b); }
function len(o) { return o.length; }
function warm(o, n, l) { for (var i = 0; i < 5000; i++) eq(len(o), n, l); }
var proto = Object.getPrototypeOf(Int8Array.prototype);
var desc = Object.getOwnPropertyDescriptor(proto, 'length');
// Several shapes at one site, then a subclass with its own getter.
var a = new Uint8Array(3), b = new Float64Array(4), c = new Int16Array(5);
for (var i = 0; i < 5000; i++) { eq(len(a), 3, 'a'); eq(len(b), 4, 'b'); eq(len(c), 5, 'c'); }
class Sub extends Uint8Array { get length() { return 'sub'; } }
var s = new Sub(2);
warm(s, 'sub', 'subclass getter');
warm(a, 3, 'a again');
class Plain extends Uint8Array {}
var p = new Plain(6);
warm(p, 6, 'plain subclass');
Object.defineProperty(Plain.prototype, 'length', {value: 'late', configurable: true});
warm(p, 'late', 'late subclass shadow');
delete Plain.prototype.length;
warm(p, 6, 'unshadowed');
// Redefine the builtin getter after compiling.
warm(a, 3, 'before redefine');
Object.defineProperty(proto, 'length', {get: function () { return 'mine'; }, configurable: true});
warm(a, 'mine', 'redefined getter');
Object.defineProperty(proto, 'length', desc);
warm(a, 3, 'restored');
// Mixed arrays and typed arrays at one site.
var arr = [1, 2];
for (var i = 0; i < 5000; i++) { eq(len(arr), 2, 'arr'); eq(len(a), 3, 'mixed'); eq(len('xyz'), 3, 'str'); }
// Reflect.construct with an array prototype that leads to the getter.
var ap = [9, 9, 9, 9];
Object.setPrototypeOf(ap, Uint8Array.prototype);
function F() {}
F.prototype = ap;
var r = Reflect.construct(Uint8Array, [2], F);
warm(r, 4, 'array prototype length');
// Detach and resize after compiling.
var buf = new ArrayBuffer(8), d = new Uint8Array(buf);
warm(d, 8, 'pre detach');
buf.transfer();
warm(d, 0, 'detached');
var rab = new ArrayBuffer(8, {maxByteLength: 16}), t = new Uint8Array(rab), f = new Uint8Array(rab, 4, 4);
warm(t, 8, 'tracking'); warm(f, 4, 'fixed');
rab.resize(6); warm(t, 6, 'shrunk'); warm(f, 0, 'oob');
rab.resize(16); warm(t, 16, 'grown'); warm(f, 4, 'back in bounds');
// Non-typed-array receivers of the getter.
var fake = Object.create(Uint8Array.prototype);
var caught = 0;
for (var i = 0; i < 3000; i++) { try { len(fake); } catch (e) { if (e instanceof TypeError) caught++; } }
eq(caught, 3000, 'fake receiver');
'ok';
