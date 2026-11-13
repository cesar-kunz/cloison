// (cloison-fuzz triage/BUGS.md #27: the script's `%this` was a global
// variable, an assignment to an undeclared one in strict code.)
// expect: true,true,true,false

var out = [];
out.push((() => this)() === globalThis);
function k() { return eval("(() => this)()"); }
out.push(k.call(5) instanceof Number);
out.push(new Function("return (() => this)()").call(3) instanceof Number);
out.push(Object.getOwnPropertyNames(globalThis).includes("%this"));
out.join(",");
