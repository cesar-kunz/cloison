// A variadic function reading arguments.length and arguments[i].
// features: loop, call, arguments
// expect: 125004500000
function sumAll() {
  var s = 0;
  for (var i = 0; i < arguments.length; i++) s += arguments[i];
  return s;
}
function main() {
  var t = 0;
  for (var i = 0; i < 500000; i++) t += sumAll(i, 1, 2) + sumAll(i & 7, 3);
  return t;
}
main();
