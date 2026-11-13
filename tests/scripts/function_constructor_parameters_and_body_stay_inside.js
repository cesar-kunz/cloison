// `Function`'s parameters and body are each a parameter list and a body:
// text in either that would end the function and go on after it is a
// `SyntaxError`, never run.
// expect: SyntaxError SyntaxError SyntaxError SyntaxError fn:3 fn:2 fn:1 fn:})} 0

var r = [], hit = 0; globalThis.evil = function () { hit++; };
function t(f) { try { var x = f(); return "fn:" + x.apply(null, [1, 2]); } catch (e) { return e.name; } }
r.push(t(() => Function("", "}); evil(); (function(){")), t(() => Function("/*", "*/){")),
  t(() => Function("a = evil()) { }, (function(", "")), t(() => Function("", "}")));
r.push(t(() => Function("a, b", "return a + b")), t(() => Function("a", "b", "return a * b")),
  t(() => Function("a //", "return a")), t(() => Function("", "return '})' + /}/.source")));
r.push(hit);
r.join(" ");
