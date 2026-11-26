// `instanceof` walks the prototype chain inline while it meets no proxy,
// and from a proxy on through its `getPrototypeOf` trap: the trap runs
// once per check that reaches it (none for `plain`, whose chain has no
// proxy), and what it returns is walked on.
// expect: true,true,false,true,false,false,103

class A {}
class B extends A {}
var traps = 0;
var P = new Proxy(Object.create(A.prototype), {
  getPrototypeOf() { traps++; return B.prototype; },
});
var o = Object.create(P);
var plain = new B();
// (A proxy itself: its trap, once.)
var other = new Proxy({}, { getPrototypeOf() { traps += 100; return null; } });
[o instanceof B, o instanceof A, o instanceof Array, plain instanceof A,
 plain instanceof Array, other instanceof A, traps].join();
