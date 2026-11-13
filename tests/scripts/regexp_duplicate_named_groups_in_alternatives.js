// A name may repeat in different alternatives (ES2025), where its groups
// can't both participate (#106): `groups` (and `indices.groups`) has it
// once, where first named, with the match of the group that matched, and
// `\k<name>` and `$<name>` are that group's. Groups that may both
// participate (in sequence, or nested) still can't share a name.
// expect: y x yy false true ab {"b":[0,1],"a":[1,2]} [y] y y z SyntaxError SyntaxError SyntaxError SyntaxError SyntaxError

var r = [/(?<a>x)|(?<a>y)/.exec("y").groups.a, new RegExp("(?<a>x)|(?<a>y)").exec("x").groups.a,
  /(?:(?<a>x)|(?<a>y))\k<a>/.exec("yy")[0], /(?:(?<a>x)|(?<a>y))\k<a>/.test("y"),
  /(?<=(?:(?<a>x)|(?<a>y))\k<a>)z/.test("yyz"), Object.keys(/(?<a>x)|(?<b>z)|(?<a>y)/.exec("y").groups).join(""),
  JSON.stringify(/(?<b>.)(?:(?<a>x)|(?<a>y))/d.exec("qy").indices.groups), "y".replace(/(?<a>x)|(?<a>y)/, "[$<a>]"),
  "y".replace(/(?<a>x)|(?<a>y)/, (...a) => a.at(-1).a), /(?:(?<a>x)|(?<a>y))+/.exec("xy").groups.a,
  /((?<a>x)|(?<b>y))|(?<a>z)/.exec("z").groups.a];
for (var src of ["(?<a>x)(?<a>y)", "(?<a>x)(?:(?<a>y)|z)", "(?:(?<a>x)|(?<a>y))(?<a>z)", "(?<a>(?<a>x))", "(?<a>x)|(?<a>(?<a>y))"]) {
  try { new RegExp(src); r.push("ok"); } catch (e) { r.push(e.name); }
}
r.join(" ");
