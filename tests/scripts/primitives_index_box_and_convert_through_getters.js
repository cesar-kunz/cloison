// A symbol's or a bigint's index is its prototype's; a sloppy function
// gets a symbol or a bigint `this` boxed; a `valueOf` that is a getter
// converts where compiled code converts.
// expect: z,s,object,object,4200

BigInt.prototype[0] = "z"; Symbol.prototype[0] = "s";
var f = function () { return typeof this; };
var o = { get valueOf() { return function () { return 7; }; } };
var s = 0;
for (var i = 0; i < 200; i++) s += +o + o * 2;
[(1n)[0], Symbol()[0], f.call(1n), f.call(Symbol()), s].join();
