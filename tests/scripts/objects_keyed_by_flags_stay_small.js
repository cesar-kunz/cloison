// An object keyed by flags, each twice the last (a TypeScript enum's
// reverse mapping, `F[F["B24"] = 1 << 24] = "B24"`): its elements kept no
// sparser than an array's, not doubled up to 2^30 (out of memory). The
// expected value is V8's.
// expect: 0,1,2,4,8 64 B24 B30 None ["0","1","5"] 8

var F = {};
for (var k = 0; k <= 30; k++) { F[F["B" + k] = 1 << k] = "B" + k; }
F[F["None"] = 0] = "None";
var o = {}; o[1] = "a"; o[5] = "b"; o[0] = "z";
[Object.keys(F).slice(0, 5).join(), Object.keys(F).length, F[1 << 24], F[1 << 30], F[0], JSON.stringify(Object.keys(o)), F.B3].join(" ");
