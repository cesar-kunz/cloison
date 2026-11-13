// A body's `var` patterns in a function with parameter defaults or
// patterns: their names are the body's variables (renamed apart from the
// parameters' scope), which functions in the body read. (TypeScript's
// `emitFiles` and `createPrinter` read them as undeclared globals.) The
// expected value is V8's and SpiderMonkey's.
// expect: 5 13 7 2,3,,5 undefined,4 6

function f(a = 1) { var { x } = { x: 5 }; return (() => x)(); }
function g({ p }) { var { enter, exit } = { enter: 1, exit: 2 }; function h() { return enter + exit + p; } return h(); }
function k(o = {}, h = {}) { const { on = 7, q } = h; function pipe() { return on + (q || 0); } return pipe(); }
function m(a = 1) { var { x, y: [z] } = { x: 2, y: [3] }; var w; return [x, z, w, (() => x + z)()].join(); }
function n(a = 1, b = () => typeof x) { var { x } = { x: 4 }; return [b(), x].join(); }
function c(a = 1) { try { throw 1; } catch (e) { var { e: q } = { e: 6 }; } return (() => q)(); }
[f(), g({ p: 10 }), k(), m(), n(), c()].join(" ");
