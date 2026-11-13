// `%` with a register divisor (`Op::Mod`): the int32 stencil, its exits
// (-0, a zero or negative divisor) and the double form after them (which
// exits for a zero divisor), and the generic one; against Node's values.
// Fails if `mod_ii` keeps a negative dividend's zero remainder.
// expect: 9492|1,1,0,0,NaN,2,2,7,NaN,3,-1,-1,-0,-0,NaN,-2,-2,-7,NaN,-3,0,0,0,0,NaN,0,0,0,NaN,0,-0,-0,-0,-0,NaN,-0,-0,-0,NaN,-0,0,0,0,0,NaN,1,1,6,NaN,2,-0,-0,-0,-0,NaN,-1,-1,-6,NaN,-2,1,1,0,0,NaN,2,2,2147483647,NaN,3,-2,-2,-0,-0,NaN,-0.5,-0.5,-2147483648,NaN,-0,2.5,2.5,0.5,0.5,NaN,0.5,0.5,5.5,NaN,1.5,-2.5,-2.5,-0.5,-0.5,NaN,-0.5,-0.5,-5.5,NaN,-1.5,0,0,0,0,NaN,0,0,1e+300,NaN,0,2,2,0,0,NaN,0.5,0.5,8,NaN,0,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN,NaN|2001|-2;-1;-3;-7;-4;-3;-4;-3;-0;-5;-0;-4;-2;-2;-0;-2;-7;-1;-4;-1;-0;-1;-1;-0;-2;-4;-0;-3;-2;-0;-2;-3;-4;-5;-1;-3;-2;-4;-4;-4;-4;-4;-0;-1;-6;-5;-4;-3;-2;-1;0;1;0;3;0;1;0;1;0;0;0;0;0;1;2;1;0;2;0;1;2;0;0;0;4;0;0;0;1;1;0;1;0;1;1;2;0;0;3;0;0;2;0;1;0;0;0;1;0;1;-2;-1;-3;-7;-4;-3;-4;-3;-0;-5;-0;-4;-2;-2;-0;-2;-7;-1;-4;-1;-0;-1;-1;-0;-2;-4;-0;-3;-2;-0;-2;-3;-4;-5;-1;-3;-2;-4;-4;-4;-4;-4;-0;-1;-6;-5;-4;-3;-2;-1;0;1;0;3;0;1;0;1;0;0;0;0;0;1;2;1;0;2;0;1;2;0;0;0;4;0;0;0;1;1;0;1;0;1;1;2;0;0;3;0;0;2;0;1;0;0;0;1;0;1;-2;-1;-3;-7;-4;-3;-4;-3;-0;-5;-0;-4;-2;-2;-0;-2;-7;-1;-4;-1;-0;-1;-1;-0;-2;-4;-0;-3;-2;-0;-2;-3;-4;-5;-1;-3;-2;-4;-4;-4;-4;-4;-0;-1;-6;-5;-4;-3;-2;-1;0;1;0;3;0;1;0;1;0;0;0;0;0;1;2;1;0;2;0;1;2;0;0;0;4;0;0;0;1;1;0;1;0;1;1;2;0;0;3;0;0;2;0;1;0;0;0;1;0;1;|4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;6;5;4;3;2;1;0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;-4;-5;-6;-0;-1;-2;-3;|0;0;0;NaN;0;0;1;NaN;0;0;2;NaN;0;0;0;NaN;0;0;1;NaN;0;0;2;NaN;0;0;0;NaN;0;0;1;NaN;0;0;2;NaN;0;0;0;

// `r % acc` with a register divisor: int32s first, then the cases that
// leave the int32 fast path (a negative dividend's zero remainder is -0,
// zero and negative divisors, INT32_MIN % -1), then doubles and strings.
function show(x) { return x === 0 && 1 / x < 0 ? "-0" : String(x); }
function ints(n, d) {
  var s = 0;
  for (var i = 0; i < n; i++) s += i % d + (i * 7) % (d + 3);
  return s;
}
function mixed(as, ds) {
  var out = [];
  for (var i = 0; i < as.length; i++)
    for (var j = 0; j < ds.length; j++) out.push(show(as[i] % ds[j]));
  return out.join(",");
}
function phases() {
  var s = "", t = 0;
  for (var k = 0; k < 3; k++) {
    for (var i = 1; i < 200; i++) t += (i * 13) % (i % 17 + 1);
    // After the loop compiled for int32s: exits, then the double form.
    for (var i = -50; i < 50; i++) {
      var d = i % 7 - 3;
      var r = i % (d === 0 ? 5 : d);
      s += show(r) + ";";
    }
  }
  return t + "|" + s;
}
// A positive divisor in a register, the dividend running down through
// negative multiples of it: -0 once the int32 form is compiled.
function negzero(d) {
  var s = "";
  for (var i = 60; i > -60; i--) s += show(i % d) + ";";
  return s;
}
// Zero divisors on the int32 path: `mod_ii` exits at the first, the
// double form at the next ones (NaN).
function zeros() {
  var s = "";
  for (var i = 1; i < 40; i++) s += show(i % (i % 4)) + ";";
  return s;
}
ints(1000, 9) + "|" + mixed([7, -7, 0, -0, 6, -6, 2147483647, -2147483648, 5.5, -5.5, 1e300, "8", NaN, Infinity],
  [3, -3, 1, -1, 0, 2.5, -2.5, Infinity, NaN, "4"]) + "|" + phases() + "|" + negzero(7) + "|" + zeros();
