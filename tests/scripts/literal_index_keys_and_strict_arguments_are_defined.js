// An object literal's index keys and a strict `arguments`' elements are
// defined, not set: a prototype's accessor for the index doesn't run
// (cloison-fuzz BUGS.md #14).
// expect: 1 1 1 a7 7 0

var hits = 0;
Object.defineProperty(Object.prototype, "7", { get() { return "G7"; }, set(v) { hits++; }, configurable: true });
var out = [({ "7": 1 })[7], ({ 7.0: 1 })[7], ({ ...{ 7: 1 } })[7],
  (function () { "use strict"; return arguments[7]; })(0, 1, 2, 3, 4, 5, 6, "a7"),
  ({ 7: function () {} })[7].name];
delete Object.prototype[7];
out.push(hits);
out.join(' ');
