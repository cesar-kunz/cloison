// A direct `eval` in an arrow of a script's top level has no `new.target`
// to see: `new.target` in its code is an early SyntaxError (#124), its
// code not run, an `eval` in it or an arrow too; a function's, in an arrow
// in its code, is the function's. SpiderMonkey's values.
// expect: SyntaxError SyntaxError SyntaxError SyntaxError ReferenceError true / true

var r = [], t = f => { try { return String(f()); } catch (e) { return e.name; } };
r.push(t(() => eval("new.target")), t(() => (() => eval("r.push('ran'); new.target"))()));
r.push(t(() => eval("eval('new.target')")), t(() => eval("(() => new.target)()")), t(() => eval("arguments")));
function F() { return [(() => eval("eval('new.target')"))(), (() => eval("(() => new.target)()"))()]; }
r.push(new F().every(x => x === F), F().join("/"), t(() => eval("this === globalThis")));
r.join(" ");
