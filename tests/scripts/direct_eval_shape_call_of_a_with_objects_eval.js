// An `eval` not the builtin, found on a `with` object, is called with
// that object as its `this` (EvaluateCall's thisValue, 13.3.6.1) and
// all the arguments: in functions and at a script's top level, with
// spreads, nested `with`s, a getter, the prototype; not one hidden by
// `Symbol.unscopables`, nor a variable's.
// expect-json: "true 1 \ntrue 2 2\ntrue 2 2\ntrue 0 \ntrue 1 /true 2 2/true 0 \ntrue 2 9\ntrue 2 9\ntrue\ntrue\ntrue\n2\ntrue\nuzuz\nbzbz\ntrue,2|true,1\ntrue\ntrue 1 "

var r = [];
var o = { eval: function (a, b) { return [this === o, arguments.length, b].join(" "); } };
with (o) r.push(eval("1"), eval(1, 2), eval(...[1, 2]), eval());
function f1() { with (o) return eval("1") + "/" + eval(1, 2) + "/" + eval(...[]); }
r.push(f1());
var p = { q: 1 }, o2 = { eval: function () { return this === o2; } };
with (o) with (p) r.push(eval("x", 9));
function f2() { with (o) with (p) return eval(...["x", 9]); }
function f3() { with (o) with (o2) return eval("1"); }
r.push(f2(), f3());
var gets = 0;
var g = { get eval() { gets++; return function () { return this === g; }; } };
function f4() { with (g) return eval(...["1"]); }
with (g) r.push(eval("1"));
r.push(f4(), gets);
var c = Object.create(o2);
o2.eval = function () { return this === c; };
function f5() { with (c) return eval("1"); }
r.push(f5());
var u = { eval: function () { return "custom"; }, z: "uz", [Symbol.unscopables]: { eval: true } };
function f6() { var z = "lz"; with (u) return eval("z") + eval(...["z"]); }
r.push(f6());
var b = { eval: eval, z: "bz" };
function f7() { var z = "lz"; with (b) return eval("z") + eval(...["z", 1]); }
r.push(f7());
function f8() { var eval = function () { return [this === globalThis, arguments.length].join(); };
  return function () { "use strict"; return eval(1, 2) + "|" + eval(...[1]); }; }
r.push(f8()());
function f9() { with (o) { let eval = function () { return this === globalThis; }; return eval("1"); } }
function f10() { with (o) return eval("eval(1, 2)"); }
r.push(f9(), f10());
r.join("\n");
