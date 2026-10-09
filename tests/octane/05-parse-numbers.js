// parseInt, parseFloat, isNaN and isFinite: on strings, and parseInt on
// numbers (box2d's parseUInt of a count).
// for: box2d (3.5M parseInt calls), pdfjs, crypto, code-load, typescript
// expect: 12,-7,255,31,35,NaN,8,42,-3,1,5,350,0.5,-Infinity,NaN,true,false,false,true
var r = [parseInt("12px"), parseInt("  -7"), parseInt("ff", 16), parseInt("0x1F"), parseInt("z", 36),
         parseInt(""), parseInt("08"), parseInt(42), parseInt(-3.9), parseInt(1e21), parseInt("101", 2),
         parseFloat("3.5e2x"), parseFloat(".5"), parseFloat("-Infinityx"), parseFloat("x"),
         isNaN("abc"), isNaN("12"), isFinite("1e400"), isFinite(7)];
r.join(",");
