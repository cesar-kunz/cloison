// A hand-written forEach/reduce taking a function argument, called with one callback per site.
// features: loop, closure, array, call
// expect: 6687080000
function reduceList(xs, f, init) {
  var acc = init;
  for (var i = 0; i < xs.length; i++) acc = f(acc, xs[i]);
  return acc;
}
function sumSquares(a, x) { return a + x * x; }
function maxOf(a, x) { return x > a ? x : a; }
function main() {
  var xs = [];
  for (var i = 0; i < 100; i++) xs.push((i * 37) % 101);
  var s = 0;
  for (var r = 0; r < 20000; r++) s += reduceList(xs, sumSquares, 0) + reduceList(xs, maxOf, 0);
  return s;
}
main();
