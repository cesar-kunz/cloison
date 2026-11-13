// Classes folding case (`i`, `iu`, `iv`) look a unit's form up among
// their short ranges' folded units, kept sorted (made at the first unit
// past ASCII tested), rather than folding them all at each test: 300
// random classes of units that fold in special ways give what V8,
// SpiderMonkey, JavaScriptCore and QuickJS do.
// expect: 36000 -897298215 12115

var seed = 7, out = [];
function rnd(n) { seed = (seed * 1103515245 + 12345) & 0x7fffffff; return seed % n; }
var pool = [0x41, 0x61, 0xb5, 0xc0, 0xdf, 0xe0, 0xff, 0x130, 0x131, 0x17f, 0x1c4, 0x1c5, 0x1c6, 0x370, 0x390, 0x3a3, 0x3c2, 0x3c3, 0x400, 0x430, 0x1e9e, 0x2126, 0x212a, 0x212b, 0x2c00, 0x2c30, 0xa640, 0xa641, 0xff21, 0xff41];
function hex(c) { return "\\u" + ("000" + c.toString(16)).slice(-4); }
for (var k = 0; k < 300; k++) {
  var src = "[" + (rnd(3) == 0 ? "^" : "");
  for (var j = rnd(3) + 1; j > 0; j--) { var lo = pool[rnd(pool.length)]; src += hex(lo); if (rnd(2)) src += "-" + hex(lo + rnd(40)); }
  src += "]";
  var bits = "";
  for (var f of ["i", "iu", "iv"]) {
    var re = new RegExp("^" + src + "$", f);
    for (var t = 0; t < 40; t++) { var c = pool[rnd(pool.length)] + rnd(3) - 1; bits += re.test(String.fromCharCode(c)) ? "1" : "0"; }
  }
  out.push(bits);
}
var h = 0; var all = out.join(""); for (var i = 0; i < all.length; i++) h = (h * 31 + all.charCodeAt(i)) | 0;
all.length + " " + h + " " + (all.split("1").length - 1)
