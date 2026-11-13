// `Float16Array`, `Math.f16round` and `DataView`'s `Float16`: binary16
// rounding from doubles, ties to even, to infinity past 65520.
// expect: 1.0009765625,1,1.0009765625,65504,Infinity,0,5.960464477539063e-8,NaN,0 true 0.0999755859375 -2.5 193 2 671.999755859375

var a = new Float16Array([1.0009765625, 1.00048828125, 1.000732421875, 65519, 65520, 1e-8, 3e-8, NaN, -0]);
var dv = new DataView(new ArrayBuffer(2));
dv.setFloat16(0, -2.5, true);
var sum = 0;
var f = new Float16Array(64);
for (var i = 0; i < 64; i++) f[i] = i / 3;
for (var j = 0; j < 64; j++) sum += f[j];
[Array.from(a).join(), Object.is(a[8], -0), Math.f16round(0.1), dv.getFloat16(0, true), dv.getUint8(1),
 Float16Array.BYTES_PER_ELEMENT, sum].join(" ");
