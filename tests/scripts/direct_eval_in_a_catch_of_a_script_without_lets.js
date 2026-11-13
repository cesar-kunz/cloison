// In a script with no block-scoped declarations but catch parameters,
// a direct `eval` in a catch clause sees the parameter apart from the
// function's `var` by its name: its block function's or `var`'s
// declaration sets the `var`, not the parameter (B.3.4; 19.2.1.3). Its
// catch parameters were left unrenamed, the function's `var`s.
// SpiderMonkey's, JSC's and V8's values.
// expect: number function 2 undefined

var log = [];
function g() { try { throw 0; } catch (ce) { eval("{ function ce() {} }"); var a = typeof ce; } return a + " " + typeof ce; }
function h() { try { throw 0; } catch (ce) { eval("var ce = 2"); var a = ce; } return a + " " + ce; }
log.push(g(), h());
log.join(" ");
