// A direct `eval`'s code is compiled for the bindings its site sees, its
// upvalues the site's bindings function's (`intrinsics::direct_eval_bound`,
// as V8 compiles it for the scopes around it): constants and the TDZ, in
// functions it makes too, closures both ways, its own `let`s, `var`s by
// their names, the same text at sites of other bindings, an `eval` in
// it, arrows, generators, `typeof`, `delete`, compound and destructuring
// assignments, `for` loops' bindings and `with` give what V8,
// SpiderMonkey, JavaScriptCore and QuickJS do.
// expect: TypeError1 TypeError1 ReferenceError,4 13,13 13,13 own,site 2 undefined undefined 3 30 3 30 6,10 6 2,11 number,false,1 ReferenceError1 11 11 15 9 TypeError,TypeError,TypeError 0,1,2 let obj

var out = [];
function err(f) { try { return String(f()); } catch (e) { return e.constructor.name; } }
// A const through a function the eval'd code makes (compiled with it, not lazily).
function c1() { const k = 1; var f = eval("(function () { k = 2; })"); return err(f) + k; }
out.push(c1(), c1());
// TDZ through a nested function, then after initialization.
function c2() { var f = eval("(function () { return t; })"); var a = err(f); let t = 4; return a + "," + f(); }
out.push(c2());
// Closures see changes both ways, many runs.
function c3() { let n = 0; var inc = eval("(function () { return ++n; })"); inc(); inc(); n += 10; return inc() + "," + n; }
out.push(c3(), c3());
// The eval'd code's own let shadows the site's binding.
function c4() { let s = "site"; return eval("let s = 'own'; s") + "," + s; }
out.push(c4());
// A var by a catch parameter's name: allowed.
function c5() { try { throw 1; } catch (q) { eval("var q = 2"); return q; } }
out.push(c5());
// A var by a let's name: an error, each time.
function c6() { let v = 1; return err(() => eval("var v = 2")); }
out.push(c6(), c6());
// The same text at two sites with other bindings.
function d1() { let a = 1, b = 2; return eval("a + b"); }
function d2() { let b = 20, a = 10; return eval("a + b"); }
out.push(d1(), d2(), d1(), d2());
// An eval in the eval'd code: sees them too.
function d3() { let x = 5; return eval("eval('x + 1')") + "," + eval("var e2 = x * 2; eval('e2')"); }
out.push(d3());
// Arrows: arguments, new.target, this through the eval.
function d4() { let w = 3; return (() => eval("arguments.length + w + (new.target === undefined)"))(); }
out.push(d4(1, 2));
// Generators and async functions with eval.
function* g1() { let y = 1; yield eval("y + 1"); y = 10; yield eval("y + 1"); }
out.push([...g1()].join());
// typeof and delete of the bindings.
function d5() { let z = 1; return eval("typeof z") + "," + eval("delete z") + "," + z; }
out.push(d5());
function d6() { return err(() => eval("typeof u")) + (function () { return 1; })(); let u = 1; }
out.push(d6());
// Compound assignment and update in the eval'd code.
function d7() { let m = 2; eval("m *= 5; m++"); return m; }
out.push(d7(), d7());
// Destructuring assignment to bindings.
function d8() { let a1 = 0, b1 = 0; eval("[a1, b1] = [7, 8]"); return a1 + b1; }
out.push(d8());
// A class binding.
function d9() { class K { static v() { return 9; } } return eval("K.v()"); }
out.push(d9());
function e1() { const C = 1; return err(() => eval("C++")) + "," + err(() => eval("C += 1")) + "," + err(() => eval("[C] = [2]")); }
out.push(e1());
// for loops with let: each iteration's binding.
function e2() { var fs = []; for (let i = 0; i < 3; i++) fs.push(eval("(function () { return i; })")); return fs.map(f => f()).join(); }
out.push(e2());
// In with.
function e3(o) { let h = "let"; with (o) { return eval("h"); } }
out.push(e3({}), e3({ h: "obj" }));
out.join(" ")
