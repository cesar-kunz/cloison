// Code in every GPR slot of both of x86-64's REX classes (rdi…rcx and
// r8, r9), shifts by a count in a slot (`shl x, cl` in rcx's own
// variant), typed-array and field stores whose `__slow` helpers come from
// templates while the stencil itself is explicit, and doubles in FP
// slots: stencils copied from family templates with their registers
// patched in. The expected values are V8's, SpiderMonkey's and QuickJS's.
// expect: 26107 2361 51133008 -339136744 1899734054 -1110494486 1270400181 77 38 115 -1110494486 -1890917326,-467097177,-490486691,428270075,-1110485231,-812972032,-212207747,-1705661033 -41145220.714286 -3.617025353508702e+30 | 11 9 188 -2 -931135487 1753221077 2 2 2 3 1753221077 22,-2147483573,134217946,-536870245,1753221086,0,0,0 6.000000 3.316488021

function mix(n) {
  var a = 1, b = 2, c = 3, d = 4, e = 5, f = 6, s = 0;
  var xs = [], ys = new Int32Array(8), zs = new Float64Array(8), o = { p: 0, q: 0 };
  for (var i = 0; i < n; i++) {
    a = (a + i) | 0; b = (b ^ a) | 0; c = (c + (b << (i & 7))) | 0;
    d = (d - (c >> (a & 15))) | 0; e = (e + (d >>> (b & 31))) | 0; f = (f * 3 + e) | 0;
    ys[i & 7] = a ^ f; zs[i & 7] = (c + d) / 7;
    if (i % 3 == 0) xs.push(e & 255);
    o.p = o.p + (a & 1); o.q = f;
    s = (s + ys[(i + 3) & 7] + (zs[(i + 5) & 7] | 0)) | 0;
  }
  var t = 0.5, u = 1.5, v = 2.5, w = 3.5, x = 4.5, y = 5.5, z = 6.5, g = 7.5;
  for (var k = 0; k < n; k++) {
    t = t * 0.5 + u; u = u * 0.25 + v; v = v - w * 0.125; w = w + x / 9;
    x = x * 0.75 + y; y = y - z * 0.5; z = z * 0.5 + g; g = g * 0.9 + t;
  }
  return [a, b, c, d, e, f, s, xs.length, xs[xs.length - 1], o.p, o.q,
          ys.join(","), zs[3].toFixed(6), (t + u + v + w + x + y + z + g).toFixed(9)].join(" ");
}
var r;
for (var j = 0; j < 30; j++) r = mix(200 + j);
r + " | " + mix(5);
