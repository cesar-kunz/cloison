// A direct `eval` sees the bindings where it is: block-scoped ones
// (catch parameters, per-iteration ones), the enclosing functions'
// variables, `new.target`; what it assigns them they keep after, and the
// `var`s it declares or assigns are the function's. (And `x op= v` in a
// `with` evaluates `v` inside it.)
// expect: let=1 const=3 block=3 catch=4 loopLet=0,1,2 assignLet=5 outer=7 outerUnused=8 closureLet=10 varDecl=11 fnDecl=12 letInEval=undefined shadow=inner param=14 arguments=2 this=15 newTarget=[object Object] classField=2 nested=16 strictVar=undefined tdz=!ReferenceError ownVarAssign=2 ownVarAssign2=2 mixed=11,20 newVar=2 outerAssign=5 deep=3 shadowVar=var typeofMissing=undefined loopAssign=3 withCompound=11

var out = [];
function t(n, f) { try { out.push(n + "=" + f()); } catch (e) { out.push(n + "=!" + e.name); } }
t("let", () => { let a = 1; return eval("a"); });
t("const", () => { const b = 2; return eval("b + 1"); });
t("block", () => { { let c = 3; return eval("c"); } });
t("catch", () => { try { throw 4; } catch (e) { return eval("e"); } });
t("loopLet", () => { var fs = []; for (let i = 0; i < 3; i++) fs.push(eval("() => i")); return fs.map(f => f()).join(); });
t("assignLet", () => { let d = 1; eval("d = 5"); return d; });
t("outer", () => { var x = 7; return (function () { return eval("x"); })(); });
t("outerUnused", () => { var y = 8; function g() { return eval("y"); } return g(); });
t("closureLet", () => { let z = 10; function h() { return eval("z"); } return h(); });
t("varDecl", () => { eval("var v = 11"); return v; });
t("fnDecl", () => { eval("function ff() { return 12; }"); return ff(); });
t("letInEval", () => { eval("let q = 13"); return typeof q; });
t("shadow", () => { let s = "outer"; { let s = "inner"; return eval("s"); } });
t("param", () => (function (p) { return eval("p"); })(14));
t("arguments", () => (function () { return eval("arguments.length"); })(1, 2));
t("this", () => ({ m() { return eval("this.k"); }, k: 15 }).m());
t("newTarget", () => { function F() { return eval("new.target === F"); } return new F(); });
t("classField", () => { class C { x = eval("1 + 1"); } return new C().x; });
t("nested", () => { let n = 16; return eval("eval('n')"); });
t("strictVar", () => { "use strict"; eval("var sv = 1"); return typeof sv; });
t("tdz", () => { return eval("tz"); let tz = 1; });
t("ownVarAssign", () => { var v = 1; let l = 0; eval("v = 2"); return v; });
t("ownVarAssign2", () => { var v = 1; eval("v = 2"); return v; });
t("mixed", () => { var v = 1; let l = 10; eval("v += l; l = 20"); return v + "," + l; });
t("newVar", () => { let l = 1; eval("var nv = l + 1"); return nv; });
t("outerAssign", () => { var ov = 1; (function () { eval("ov = 5"); })(); return ov; });
t("deep", () => { let a = 1; return (function () { let b = 2; return (function () { return eval("a + b"); })(); })(); });
t("shadowVar", () => { let s = "lex"; return (function () { var s = "var"; return eval("s"); })(); });
t("typeofMissing", () => { return eval("typeof notDefinedAnywhere"); });
t("loopAssign", () => { let sum = 0; for (let i = 0; i < 3; i++) eval("sum += i"); return sum; });
var o = { v: 1, l: 10 }; with (o) { v += l } out.push('withCompound=' + o.v);
out.join(' ');
