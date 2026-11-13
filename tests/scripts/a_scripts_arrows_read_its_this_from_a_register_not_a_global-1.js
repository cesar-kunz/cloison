// (cloison-fuzz triage/BUGS.md #27: the script's `%this` was a global
// variable, an assignment to an undeclared one in strict code.)
// expect: true,true,true,true,true,true,true,true,true,true,true,false

"use strict";
var out = [];
out.push((() => this)() === globalThis);
out.push((() => () => this)()() === globalThis);
out.push((() => eval("this"))() === globalThis);
out.push(eval("(() => this)()") === globalThis);
out.push((0, eval)("(() => this)()") === globalThis);
function f() { return (() => this)(); }
out.push(f.call(7) === 7, f() === undefined);
class C { x = (() => this)(); static s = (() => this)(); }
out.push(new C().x instanceof C, C.s === C);
{ let t = (() => this)(); out.push(t === globalThis); }
out.push([1].map(() => this)[0] === globalThis);
out.push(Object.getOwnPropertyNames(globalThis).includes("%this"));
out.join(",");
