// A double stored into a register with an int32 home is kept boxed
// (`star_u<k>_v`; there is no `star_u<k>_f`).
// expect: -1471031552,-1750991872

function f(n) {
  var a = -2147483648, b = -7, d = NaN, h = 0;
  for (var i = 0; i < n; i++) {
    b = ((b * 5) - Math.round(2)); b = (d - b); a |= 4294967296; b = (b | 0);
    h = (h * 31 + (b !== b ? 3 : 2)) | 0;
  }
  return h;
}
f(3000) + "," + f(300000);
