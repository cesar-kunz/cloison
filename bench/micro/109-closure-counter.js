// A closure reading and writing a captured variable (a generator by hand).
// features: loop, closure, call
// expect: 202122944
function makeCounter(step) {
  var n = 0;
  return function () { n += step; return n; };
}
function main() {
  var next = makeCounter(3), s = 0;
  for (var i = 0; i < 2000000; i++) s = (s + next()) & 0xfffffff;
  return s;
}
main();
