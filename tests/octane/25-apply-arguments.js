// `g.apply(x, arguments)` in a function that reads `arguments` only that
// way (Prototype's `Class.create`): no `arguments` object, the frame's
// arguments passed on, also through two such functions, to a callee that
// reads its own `arguments`; `apply` replaced, a native `g`, a `null`
// `this`; and functions that read `arguments` otherwise too.
// for: raytrace
// expect: 3|1,2,|7,8,9|9|5:1,2,3,4,5|2|global|over:2:a,b|7|4|1:x|1:x|3:x,b,c|err|50015000|10000|7,1
var Class = {
  create: function () {
    return function () { this.initialize.apply(this, arguments); };
  }
};
var P = Class.create();
P.prototype = { initialize: function (a, b, c) { this.v = [a, b, c]; this.n = arguments.length; } };
var Q = Class.create();
Q.prototype = { initialize: function () { P.prototype.initialize.apply(this, arguments); } };
function all() { return arguments.length + ":" + Array.prototype.join.call(arguments, ","); }
function fwd() { return all.apply(this, arguments); }
function fwd2() { return fwd.apply(this, arguments); }
function count() { return arguments.length; }
function countFwd() { return count.apply(null, arguments); }
var global = this;
function who() { return this === global ? "global" : "other"; }
function whoFwd() { return who.apply(null, arguments); }
function g(a, b) { return a + b; }
g.apply = function (that, args) { return "over:" + args.length + ":" + args[0] + "," + args[1]; };
function overFwd() { return g.apply(this, arguments); }
function maxFwd() { return Math.max.apply(Math, arguments); }
// Reads `arguments` otherwise too: an object as before.
function mixed() { return arguments.length + count.apply(this, arguments); }
function named(arguments) { return all.apply(this, arguments); }
function shadow() { var arguments = ["x"]; return all.apply(this, arguments); }
function both() { arguments[0] = "x"; return arguments.length + all.apply(this, arguments).slice(1); }
function notCallable() { var o = {}; return o.f.apply(this, arguments); }
var p = new P(1, 2, 3), q = new Q(1, 2);
var out = [p.n, q.v.join(","), new P(7, 8, 9, 10).v.join(","), countFwd(1, 2, 3, 4, 5, 6, 7, 8, 9),
  fwd2(1, 2, 3, 4, 5), countFwd(undefined, null), whoFwd(), overFwd("a", "b"), maxFwd(3, 7, 1),
  mixed(1, 2), named(["x"]), shadow(), both("a", "b", "c")];
try { notCallable(1); out.push("ok"); } catch (e) { out.push("err"); }
// Hot: compiled, `new` and calls through the runtime.
var s = 0, n = 0;
for (var i = 0; i < 10000; i++) {
  var o = new P(i, i, i);
  s += o.v[0] + countFwd(i, i);
  n += o.n === 3 ? 1 : 0;
}
var r = new Q(7, 1);
out.push(s, n, r.v.slice(0, 2).join(","));
out.join("|");
