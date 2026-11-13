// `instanceof` calls a `Symbol.hasInstance` method, `Function.prototype`'s
// fixed one OrdinaryHasInstance; a date converts by its own
// `Symbol.toPrimitive`; `with` leaves out what `Symbol.unscopables` names,
// an array's `keys`, `values`, … included.
// expect: true false true true false TypeError true false 0 string TypeError outer 1 function out

var r = [];
class E { static [Symbol.hasInstance](n) { return n % 2 === 0; } }
r.push(2 instanceof E, 3 instanceof E, Function.prototype[Symbol.hasInstance].call(Array, []));
var o = { [Symbol.hasInstance]: v => v === 1 }; r.push(1 instanceof o, 2 instanceof o);
try { ({}) instanceof {}; } catch (e) { r.push(e.name); }
function F() {} r.push(new F() instanceof F.bind(null), 1 instanceof Object.create(Function.prototype));
var d = new Date(0);
r.push(d[Symbol.toPrimitive]("number"), typeof d[Symbol.toPrimitive]("default"));
try { d[Symbol.toPrimitive]("bogus"); } catch (e) { r.push(e.name); }
var keys = "outer", values = 1; with ([]) { r.push(keys, values, typeof push); }
var x = "out"; with ({ x: "in", [Symbol.unscopables]: { x: true } }) { r.push(x); }
r.join(" ");
