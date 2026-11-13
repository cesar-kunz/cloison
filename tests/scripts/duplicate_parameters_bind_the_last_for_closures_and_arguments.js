// A sloppy function's duplicate parameters: the last of a name binds it,
// for the closures that capture it too, and its mapped `arguments` maps
// each name to the last index that has it, the earlier ones not mapped
// (#178; ECMA-262 FunctionDeclarationInstantiation and
// CreateMappedArgumentsObject). Closures read the first's register and
// the object wasn't mapped at all. With fewer arguments, a direct
// `eval`, in a generator (whose parameters are in its scope object), and
// compiled. What SpiderMonkey and JavaScriptCore give.
// expect: 2 9,2 5 9 1,7 2,9 1,undefined 4 1,5 7 undefined,undefined 9 9 12517500 30000

var out = [];
function d1(a, a) { return (() => a)(); } out.push(d1(1, 2));
function d2(a, b, a) { var k = () => a; a = 9; return k() + "," + b; } out.push(d2(1, 2, 3));
function d3(a, a) { var k = (v) => { a = v; }; k(5); return a; } out.push(d3(1, 2));
function d4(a, a) { arguments[1] = 9; return a; } out.push(d4(1, 2));
function d5(a, a) { a = 7; return arguments[0] + "," + arguments[1]; } out.push(d5(1, 2));
function d6(a, a) { arguments[0] = 9; return a + "," + arguments[0]; } out.push(d6(1, 2));
function d7(a, a) { a = 3; return arguments.length + "," + arguments[1]; } out.push(d7(1));
function d8(a, a) { var k = () => a; arguments[1] = 4; return k(); } out.push(d8(1, 2));
function d9(a, a) { eval("a = 5"); return arguments[0] + "," + arguments[1]; } out.push(d9(1, 2));
function d10(a, b, a, b) { return (() => a + b)(); } out.push(d10(1, 2, 3, 4));
function d11(x, x, x) { return (() => x)() + "," + x; } out.push(d11(1, 2));
function* d12(a, a) { arguments[1] = 9; yield a; arguments[0] = 8; yield a; } var g = d12(1, 2); out.push(g.next().value, g.next().value);
function h(a, b, a) { var k = () => a; a = a + 1; return k() + b; }
function h2(a, a) { a = a + 1; arguments[0] = 0; return arguments[1] + a; }
var s = 0, t = 0;
for (var i = 0; i < 5000; i++) { s += h(1, i, 3); t += h2(i, 2); }
out.push(s, t);
out.join(" ")
