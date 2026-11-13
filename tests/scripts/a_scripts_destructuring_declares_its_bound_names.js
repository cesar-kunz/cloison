// A script's top-level destructuring declares its BoundNames, nested,
// defaulted and rest ones too, and no name `""` (16.1.7: #126).
// expect: 1 2 1 3 2 4 5 6 true false SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError

let {a, x: [b, {c = 1}, ...d], ...e} = {a: 1, x: [2, {}, 3]};
$262.evalScript("let {f} = {f: 2}; const [q] = [4];");
var [h, {i}] = [5, {i: 6}];
var r = [a, b, c, d, f, q, h, i, "i" in globalThis, "a" in globalThis];
for (const s of ["var a", "let a", "var e", "let d", "var c", "const {b} = {}", "let f", "var q", "let h", "let i"])
  try { $262.evalScript(s); r.push("ok"); } catch (x) { r.push(x.name); }
r.join(" ");
