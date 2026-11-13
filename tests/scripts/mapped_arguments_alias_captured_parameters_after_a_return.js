// A sloppy function's mapped `arguments` and its parameters stay linked
// after it returns (#163): a closure that captured a parameter and the
// `arguments` object see each other's writes, as ECMA-262's MakeArgGetter
// and MakeArgSetter close over the function's environment. They copied
// the values at the return and went separate ways. The issue's
// reproducer: what SpiderMonkey and JavaScriptCore give (QuickJS cuts the
// link too).
// expect: 2 2 5

function f(a) { return [function () { return a; }, arguments]; }
var r = f(1);
r[1][0] = 2;
function h(a) { var k = function (v) { a = v; }; return [k, arguments]; }
var q = h(1);
q[0](5);
[r[0](), r[1][0], q[1][0]].join(" ")
