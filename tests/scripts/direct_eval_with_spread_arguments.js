// `eval(...args)` is a direct `eval` too (13.3.6.1), of the first
// argument after the spread; and an `eval` not the builtin, called so or
// with several arguments, gets them all.
// expect: local,let,undefined,undefined,5,global

var x = "global", r = [];
function f() { var x = "local"; return eval(...["x"]); }
r.push(f());
function g() { let x = "let"; return eval(...["x", 1], ...[2]); }
r.push(g());
function h() { "use strict"; eval(...["var sv = 1"]); return typeof sv; }
r.push(h());
function k() { return eval(...[]); }
Array.prototype[0] = "x";
r.push(String(k()));
delete Array.prototype[0];
function m() { var eval = function () { return arguments.length; }; return eval(1, 2, 3) + eval(...[1, 2]); }
r.push(m());
r.push(eval(...["x"]));
r.join();
