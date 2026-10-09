// Typed arrays: conversions on store, length, views sharing a buffer,
// subarray, set.
// for: gbemu (21M length reads), mandreel, zlib, pdfjs
// expect: 44,255,1,,-56,-1,0,0.1,0.10000000149011612,4464,4294967295,4,1,2,2,0-0-7-8-0,4,8,1498500
var u8 = new Uint8Array(4), i8 = new Int8Array(4), i32 = new Int32Array(3), f64 = new Float64Array(2);
var f32 = new Float32Array(1), u16 = new Uint16Array(1), u32 = new Uint32Array(1);
u8[0] = 300; u8[1] = -1; u8[2] = 1.9; i8[0] = 200; i32[0] = 4294967295; f64[1] = 0.1;
f32[0] = 0.1; u16[0] = 70000; u32[0] = -1;
var buf = new ArrayBuffer(8), a = new Int32Array(buf), b = new Uint8Array(buf);
a[0] = 0x01020304;
var s = new Uint8Array([1, 2, 3, 4, 5]).subarray(1, 3);
var t = new Int32Array(5);
t.set([7, 8], 2);
var sum = 0, big = new Int32Array(1000);
for (var i = 0; i < big.length; i++) big[i] = i * 3;
for (var j = 0; j < big.length; j++) sum += big[j];
[u8[0], u8[1], u8[2], u8[9], i8[0], i32[0], f64[0], f64[1], f32[0], u16[0], u32[0], b[0], b[3],
 s.length, s[0], t.join("-"), u8.length, buf.byteLength, sum].join(",");
