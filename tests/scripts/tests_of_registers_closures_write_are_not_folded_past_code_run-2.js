// As above, with the write elsewhere between the store and the test: a
// getter's, a `valueOf`'s an untyped `+` calls, and an inlined
// closure's after int arithmetic (whose stencils keep what the test
// knows) (#64).
// expect: 3000

function g() {
  var b, c = 0, s = 0;
  var v = (() => ({ valueOf() { b = false; return 1; } }))();
  for (var i = 0; i < 3000; i++) {
    b = true;
    s = s + v;
    if (b) c++;
  }
  return c * 10000 + s;
}
g();
