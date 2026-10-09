// The `in` operator: own, inherited, missing, array indices.
// for: earley-boyer, pdfjs, mandreel, zlib
// expect: true,true,false,true,false,true,true
function C() { this.a = 1; }
C.prototype.b = 2;
var c = new C(), arr = [5, 6];
["a" in c, "b" in c, "z" in c, 0 in arr, 2 in arr, "length" in arr, "push" in arr].join(",");
