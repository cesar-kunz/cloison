// As above, with the write elsewhere between the store and the test: a
// getter's, a `valueOf`'s an untyped `+` calls, and an inlined
// closure's after int arithmetic (whose stencils keep what the test
// knows) (#64).
// expect: 4498500

function g() {
  var b, c = 0, s = 0;
  var clr = (() => () => { b = false; })();
  for (var i = 0; i < 3000; i++) {
    b = true;
    s = (s + i) | 0;
    clr();
    s = s ^ 1;
    if (b) c++;
  }
  return c * 100000000 + s;
}
g();
