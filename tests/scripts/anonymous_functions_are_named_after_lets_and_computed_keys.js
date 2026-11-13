// An anonymous function or class is named after what it is assigned to:
// a renamed `let`'s source name (not `a%0`), a computed key's value (a
// symbol's in brackets), but a class's own static `name`.
// expect: ["a","y","w","d","comp","[d]","2","c","function","named",""]

var r = [];
let a; a = () => {}; r.push(a.name);
let y; y ??= function () {}; r.push(y.name);
{ let w; w = function () {}; r.push(w.name); }
let d; d = class {}; r.push(d.name);
var o = { ["comp"]: () => {}, [Symbol("d")]: function () {}, [1 + 1]: () => {}, ["c"]: class {},
  ["s"]: class { static name() {} }, ["n"]: function named() {}, ["v"]: (0, function () {}) };
r.push(o.comp.name, o[Object.getOwnPropertySymbols(o)[0]].name, o[2].name, o.c.name, typeof o.s.name, o.n.name, o.v.name);
JSON.stringify(r);
