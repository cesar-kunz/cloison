// A hot function with a try/catch whose catch rarely runs.
// features: loop, call, throw
// expect: 3739010500
function checkedDiv(a, b) {
  if (b == 0) throw "div0";
  return (a / b) | 0;
}
function safeDiv(a, b) {
  try {
    return checkedDiv(a, b);
  } catch (e) {
    return -1;
  }
}
function main() {
  var s = 0;
  for (var i = 0; i < 1000000; i++) s += safeDiv(i, i % 1000);
  return s;
}
main();
