// `>`, `<=` and `>=` on numbers, one a double, compare as doubles in
// compiled code (`tgt_jf_ff_g0`, …): a NaN makes each false, so the
// negated jump is taken; -0 equals 0; ints and doubles mix; loop tests
// jump on true. The expected value is what V8, SpiderMonkey and
// JavaScriptCore print.
// expect: 874315 548637 773158 668 2 6

var vals = [0, -0, 0.5, -0.5, 1, 2, -1, 1.5, NaN, Infinity, -Infinity, 2147483647, -2147483648, 2147483647.5, 1e300, -1e-300];
function run(a, b) {
  var c = 0;
  for (var i = 0; i < 256; i++) {
    var x = a[i & 15], y = b[i >> 4];
    if (x > y) c += 1; else c += 2;
    if (x <= y) c += 4; else c += 8;
    if (x >= y) c += 16; else c += 32;
    c = c * 3 % 1000003;
  }
  return c;
}
function ints(a, b) { var c = 0; for (var i = 0; i < 256; i++) { var x = a[i & 15] | 0, y = b[i >> 4]; if (x > y) c += 1; if (x <= y) c += 3; if (y >= x) c += 7; c = c * 5 % 1000003; } return c; }
function loop(lim, step) { var x = 0.25, n = 0; do { x += step; n++; } while (lim > x && n < 5000); do { x -= step; n++; } while (lim <= x && n < 10000); return n; }
[run(vals, vals), ints(vals, vals), ints(vals.slice().reverse(), vals), loop(500, 0.75), loop(NaN, 1), loop(-0.5, 0.25)].join(" ");
