// A promise's `then` and `finally` make theirs by its constructor's
// `Symbol.species` (null or undefined: `Promise`; else a constructor).
// expect: false false true TypeError 1

var r = [];
class S extends Promise { static get [Symbol.species]() { return Promise; } }
r.push(S.resolve(1).then(() => {}) instanceof S, S.resolve(1).finally(() => {}) instanceof S);
class T extends Promise {} r.push(T.resolve(1).then(() => {}) instanceof T);
var p = Promise.resolve(1); p.constructor = { [Symbol.species]: 5 }; try { p.then(); r.push("no"); } catch (e) { r.push(e.name); }
var calls = 0; function C(ex) { calls++; return new Promise(ex); } var p4 = Promise.resolve(1); p4.constructor = { [Symbol.species]: C }; p4.then(); r.push(calls);
r.join(" ");
