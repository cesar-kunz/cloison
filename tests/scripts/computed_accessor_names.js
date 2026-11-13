// Computed getters and setters are named `get k`/`set k` (#140), in
// literals and classes; literal keys keep theirs.
// expect: get ab|set ab|get lit|get [q]|get 1|get ab|set ab|get x|set |1,ab,lit||

var k = "ab", s = Symbol("q"), e = Symbol(), r = [];
var g = (o, p, w) => Object.getOwnPropertyDescriptor(o, p)[w].name;
var o = { get [k]() {}, set [k](v) {}, get lit() {}, get [s]() {}, get [1]() {} };
r.push(g(o, "ab", "get"), g(o, "ab", "set"), g(o, "lit", "get"), g(o, s, "get"), g(o, 1, "get"));
var c = class { static get [k]() {} set [k](v) {} get x() {} static set [e](v) {} };
r.push(g(c, "ab", "get"), g(c.prototype, "ab", "set"), g(c.prototype, "x", "get"), g(c, e, "set"));
r.push(Object.keys(o), Object.keys(c), Object.keys(c.prototype));
r.join("|");
