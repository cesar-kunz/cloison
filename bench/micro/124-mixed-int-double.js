// Arithmetic on values that are ints or doubles at the same site (every third is a double).
// features: loop, double, array, call
// expect: 339390000
function blend(a, b) { return a * 3 + b - (a >> 1); }
function main() {
  var vs = [];
  for (var i = 0; i < 99; i++) vs.push(i % 3 == 0 ? i + 0.5 : i);
  var s = 0;
  for (var r = 0; r < 20000; r++)
    for (var i = 1; i < vs.length; i++) s += blend(vs[i], vs[i - 1]);
  return Math.round(s);
}
main();
