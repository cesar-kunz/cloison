// expect: [object TNumber] [object TBoolean] [object TString] [object TSymbol] [object TBigInt] [object Null] [object Undefined] object,object,object,object,object

"use strict";
var out = [], seen = [];
for (var [P, v] of [[Number, 1], [Boolean, true], [String, "s"], [Symbol, Symbol()], [BigInt, 1n]]) {
  Object.defineProperty(P.prototype, Symbol.toStringTag,
    { get() { seen.push(typeof this); return "T" + P.name; }, configurable: true });
  out.push(Object.prototype.toString.call(v));
}
out.push(Object.prototype.toString.call(null), Object.prototype.toString.call(undefined));
out.join(" ") + " " + seen.join(",");
