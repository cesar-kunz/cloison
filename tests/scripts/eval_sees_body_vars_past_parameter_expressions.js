// A direct `eval` in a function with parameter expressions sees, sets
// and redeclares the body's `var`s (renamed apart from the parameters'
// scope), from nested functions too (cloison-fuzz BUGS.md #12).
// expect: 6 6 5 7 function 4 3 3 false

var out = [];
out.push((function (p = 1) { var l = 3; return eval("l * 2"); })());
out.push((function ({ x }) { var l = 3; return eval("l * 2"); })({}));
out.push((function (p = 1) { var l = 3; eval("l = 5"); return l; })());
out.push((function (p = 1) { var l = 3; eval("var l = 7"); return l; })());
out.push((function (p = 1) { var l = 3; eval("function l() {}"); return typeof l; })());
out.push((function (p = 1) { var l = 3; var g = eval("() => l"); l = 4; return g(); })());
out.push((function (p = 1) { var l = 3; function g() { return eval("l"); } return g(); })());
out.push((function (p = 1) { var l = 3; return (() => eval("l"))(); })());
out.push((function (p = 1) { var l = 3; return eval("delete l"); })());
out.join(' ');
