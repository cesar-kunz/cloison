// `Object.freeze(arguments)` defines each index non-writable, which
// unmaps it from its parameter, the element keeping the parameter's
// value then (the arguments object's [[DefineOwnProperty]]), as
// `defineProperty` with `writable: false` already did; `seal` keeps the
// mapping. Assignments to the parameters after, `length`, descriptors,
// a strict function's and a compiled one's give what V8, SpiderMonkey,
// JavaScriptCore and QuickJS do. (#8)
// expect: 1|5 7|5,2,7,8,true|9,4,9,4|2 {"value":2,"writable":false,"enumerable":true,"configurable":false}|1|1 1|2 10,20|5,2,7,8,true

var out = [];
function f1(a) { Object.freeze(arguments); a = 3; return arguments[0]; }
function f2(a) { a = 5; Object.defineProperty(arguments, "0", { writable: false }); a = 7; return arguments[0] + " " + a; }
function f3(a, b) { a = 5; Object.freeze(arguments); a = 7; b = 8; return [arguments[0], arguments[1], a, b, Object.isFrozen(arguments)].join(); }
function f4(a, b) { Object.seal(arguments); a = 9; arguments[1] = 4; return [arguments[0], arguments[1], a, b].join(); }
function f5(a) { a = 2; Object.freeze(arguments); return Object.getOwnPropertyDescriptor(arguments, "0").value + " " + JSON.stringify(Object.getOwnPropertyDescriptor(arguments, "0")); }
function f6(a, b) { "use strict"; Object.freeze(arguments); a = 3; return arguments[0]; }
function f7(a) { var g = function () { return arguments; }; Object.freeze(arguments); try { arguments[0] = 4; } catch (e) { return "threw"; } return arguments[0] + " " + a; }
function f8(a, b, c) { Object.freeze(arguments); a = 1; b = 2; c = 3; return arguments.length + " " + [].join.call(arguments); }
out.push(f1(1), f2(1), f3(1, 2), f4(1, 2), f5(1), f6(1), f7(1), f8(10, 20));
for (var i = 0; i < 2000; i++) out[8] = f3(i, 2);
out.join("|")
