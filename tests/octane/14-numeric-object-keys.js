// Integer keys in object literals: element lookups, for-in order.
// for: earley-boyer, pdfjs
// expect: a,b,d,1;2;177;x;
var o = { 177: "a", 1: "b", x: "c", 2: "d" };
var keys = "";
for (var k in o) keys += k + ";";
[o[177], o["1"], o[2], keys].join(",");
