// A typed array's shape says its kind until one gets another kind's
// prototype (`Vm::typed_protos_crossed`): from then on compiled code
// checks each one's kind, not reading a Uint8Array's bytes as doubles.
// expect: 0;4080;5;0000000000000000 255255255255255255255255255255255255255255255255 0000000000000000 0000000000000000

var real = new Float64Array(4);
function wr(a, v) { for (var i = 0; i < a.length; i++) a[i] = v; }
function rd(a) { var s = 0; for (var i = 0; i < a.length; i++) s += a[i]; return s; }
for (var k = 0; k < 3000; k++) { wr(real, 1.25); rd(real); }
var fake = Reflect.construct(Uint8Array, [16], Float64Array);
var r = [rd(fake)];
var bufs = []; for (var j = 0; j < 4; j++) bufs.push(new ArrayBuffer(16));
var victim = new Uint8Array(bufs[1]);
Object.setPrototypeOf(victim, Float64Array.prototype);
wr(victim, -1);
r.push(rd(victim), rd(real), bufs.map(b => new Uint8Array(b).join("")).join(" "));
r.join(";");
