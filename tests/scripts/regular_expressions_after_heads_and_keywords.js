// A `/` starts a regular expression after a statement's head, a prefix
// `++`, `yield`, `await` and `for … of`, and divides after an operand.
// expect: if for of:b of:b re g 1 3 3 3.5 1 2 3

var r = [];
if (true) /a/.test("a") && r.push("if");
for (var k = 0; k < 1; k++) /a/.test("a") && r.push("for");
for (var x of /a/g[Symbol.split]("bab")) r.push("of:" + x);
function* g() { yield /re/; yield /x/g.flags; }
var it = g(); r.push(it.next().value.source, it.next().value);
var a = 6, b = 2, c = 3; r.push(a / b / c, (a) / b, a++ / 2, a-- / 2);
var n = 1
++/a/.lastIndex
r.push(n, [4][0] / 2, ((x) => x)(9) / 3);
r.join(" ");
