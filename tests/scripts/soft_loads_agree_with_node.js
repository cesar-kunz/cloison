// Loads whose caches missed in compiled code (soft: a miss goes through
// the runtime and on) meeting new shapes, an int32 field holding a double,
// an array, and a getter (which the runtime leaves to the interpreter).
// expect: 16640798

function P(x) { this.x = x; this.d = x * 0.5; this.o = { k: x }; }
P.prototype.m = function () { return this.x + 1; };
function Q(x) { this.q = 0; this.x = x; this.d = x + 0.25; this.o = { k: -x }; }
Q.prototype.m = function () { return this.x * 2; };
function R(x) { this.r = 0; this.q = 0; this.x = x; this.d = x; this.o = { k: 1 }; }
R.prototype.m = function () { return 7; };
function G(x) { this.g = 0; this.d = 1.5; this.o = { k: 2 }; }
Object.defineProperty(G.prototype, "x", { get: function () { return 100; } });
G.prototype.m = function () { return 3; };
function f(o) { return o.x + o.d + o.o.k + o.m(); }
function main() {
  var objs = [0];
  for (var i = 0; i < 50; i++) objs.push(new P(i));
  var t = 0;
  for (var r = 0; r < 4000; r++) {
    for (var i = 1; i < objs.length; i++) t += f(objs[i]);
    if (r == 1000) for (var i = 0; i < 50; i += 3) objs[i] = new Q(i);
    if (r == 2000) for (var i = 1; i < 50; i += 5) objs[i] = new R(i);
    if (r == 3000) for (var i = 2; i < 50; i += 7) objs[i] = new G(i);
    if (r == 3500) {
      objs[4] = { x: 0.75, d: 1, o: { k: 1 }, m: function () { return 0; } };
      objs[5] = [1, 2]; objs[5].x = 1; objs[5].d = 2; objs[5].o = { k: 3 };
      objs[5].m = function () { return this.length; };
    }
  }
  return t;
}
main();
