// A direct `eval` in a `with` at a script's top level sees the `with`
// objects, innermost first; its `var`s are still globals.
// expect: 5 number 6 false 7 true undefined 9

var r = [];
var o = { m: 5 }, p = { m: 7 };
with (o) r.push(eval("m"));
with (o) eval("var q = 1; m = 6");
r.push(typeof q, o.m, "q" in o);
with (o) with (p) r.push(eval("m"));
with (o) r.push(eval("this") === globalThis);
r.push(eval("typeof m"));
let l = 3;
with (o) r.push(eval("l + m"));
r.join(" ");
