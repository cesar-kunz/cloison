// `x op= v` reads `x` before `v`, also when `v` changes `x` through a call
// (a closure capturing it, a getter, `valueOf`): `out += render(child)`
// keeps what `render` appended.
// expect: 1 19 3 2 4 3

var r = [];
function f() { var x = 1; function g() { x = 100; return 0; } x += g(); return x; } r.push(f());
function f2() { var x = 10; function g() { x = 100; return 1; } x -= g(); x *= (g(), 2); x |= g(); return x; } r.push(f2());
function f3() { let x = 1; const o = { get p() { x = 50; return 2; } }; x += o.p; return x; } r.push(f3());
function f4(a) { function g() { a = 7; return 1; } a += g(); return a; } r.push(f4(1));
function f5() { var x = 1; var o = { valueOf() { x = 9; return 3; } }; x += o; return x; } r.push(f5());
var out = ""; function render(n) { if (n > 0) out += render(n - 1); out += n; return ""; }
render(3); r.push(out);
r.join(" ");
