// A direct `eval` in an arrow sees the enclosing function's `arguments`
// and `new.target`, as the arrow does.
// expect: 8 true 3 3

var r = [(function () { return (() => eval("arguments[0]"))(); })(8)];
function F() { this.v = (() => eval("new.target"))() === F; } r.push(new F().v);
function G() { return (() => (() => eval("arguments.length"))())(); } r.push(G(1, 2, 3));
r.push((function () { return (() => eval("this.k"))(); }).call({ k: 3 }));
r.join(" ");
