// A register set to `true`, then cleared by an inlined closure (made by
// a nested function, so inlined) through its upvalue before it is tested
// (#64): the test isn't folded on the value stored.
// expect: 0

function g() {
  var b, c = 0;
  var clr = (() => () => { b = false; })();
  for (var i = 0; i < 3000; i++) {
    b = true;
    clr();
    if (b) c++;
  }
  return c;
}
g();
