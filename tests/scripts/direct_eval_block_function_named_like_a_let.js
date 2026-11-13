// A function declared in a block of a direct `eval`'s code whose name a
// block-scoped binding between the `eval` and its function's scope has
// is not hoisted, and no error: EvalDeclarationInstantiation's
// `bindingExists` (19.2.1.3 step 11). One by a catch parameter's name
// is, but the parameter is untouched (B.3.4). A `var` by the name is
// still an error (#158).
// expect: fn:number fn-scope:number script:number undefined switch:number undefined outer-let:number catch:number function catch-scope:number function global-catch:number function var-too:number arrow:number with:number inside:number hoisted:function enclosing-fn:function loop:functionundefinedfunction var:SyntaxError var-scope:SyntaxError

var log = [];
function t(name, f) { try { log.push(name + ":" + f()); } catch (e) { log.push(name + ":" + e.name); } }
t("fn", function () { { let l = 1; eval('{ function l() {} }'); return typeof l; } });
t("fn-scope", function () { { let l = 1; eval('{ function l() {} } // eval'); return typeof l; } });
{ let l2 = 1; eval('{ function l2() {} }'); log.push("script:" + typeof l2 + " " + typeof globalThis.l2); }
{ const k = 1; eval('switch (1) { case 1: function k() {} }'); log.push("switch:" + typeof k + " " + typeof globalThis.k); }
t("outer-let", function () { let q = 1; { eval('{ function q() {} }'); } return typeof q; });
t("catch", function () { try { throw 0 } catch (ce) { eval('{ function ce() {} }'); var a = typeof ce; } return a + " " + typeof ce; });
t("catch-scope", function () { try { throw 0 } catch (ce) { eval('{ function ce() {} } // eval'); var a = typeof ce; } return a + " " + typeof ce; });
try { throw 0 } catch (g) { eval('{ function g() {} }'); log.push("global-catch:" + typeof g + " " + typeof globalThis.g); }
t("var-too", function () { var w = 5; { let w = 1; eval('{ function w() {} }'); } return typeof w; });
t("arrow", () => { { let b = 1; eval('{ function b() {} }'); return typeof b; } });
t("with", function () { { let c = 1; with ({}) { eval('{ function c() {} }'); } return typeof c; } });
t("inside", function () { { let d = 1; return eval('{ function d() {} } typeof d'); } });
t("hoisted", function () { { eval('{ function p() {} }'); } return typeof p; });
t("enclosing-fn", function () { let z = 1; return (function () { eval('{ function z() {} }'); return typeof z; })(); });
t("loop", function () { var s = ""; for (var i = 0; i < 3; i++) { if (i == 1) { let r = 1; eval('{ function r() {} }'); s += typeof r[0]; } else { eval('{ function r() {} }'); s += typeof r; } } return s; });
t("var", function () { { let v = 1; eval('var v'); } return "no"; });
t("var-scope", function () { { let v = 1; eval('var v; // eval'); } return "no"; });
log.join(" ");
