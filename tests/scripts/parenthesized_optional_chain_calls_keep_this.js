// Parentheses keep an optional chain's reference: `(o?.f)()` calls `f`
// with `o` as `this`, like `(o.f)()`; if the chain ends early, the call
// throws a `TypeError` after evaluating its arguments.
// expect: true true true true true true true true false true true 1  1

var o = { f() { return this === o }, a: { f() { return this === o.a } } }, k = "f";
function t() { return this === o } o.t = t;
var r = [(o?.f)(), (o?.["f"])(), (o?.[k])(), (o?.a.f)(), (o?.a?.f)(), (o?.f)?.(), (o?.f)(...[1]),
  (o?.t)`x`, (0, o?.f)()];
var n = null, log = [];
try { (n?.f)(log.push(1)) } catch (e) { r.push(e instanceof TypeError) }
try { (n?.[log.push(2)])(...[]) } catch (e) { r.push(e instanceof TypeError) }
r.push(log.join(), (n?.f)?.(log.push(3)), log.length);
r.join(" ");
