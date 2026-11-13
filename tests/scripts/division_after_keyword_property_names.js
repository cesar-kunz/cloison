// A `/` after a keyword that is a property name (after `.` or `?.`)
// divides: `o.if / 2`, `o?.yield / 2`, `o.of / 2`, `o.if(x) / 2`.
// expect: 5 5 4 3 2 10 7 8 11 14 4 3 11 3 20 4 1 b b g 2 true object

var o = { if: 10, yield: 8, await: 6, of: 4, default: 20, in: 14, return: 16, new: 22, class: 28,
  while(x) { return x } };
var r = [o.if / 2, o?.if / 2, o.yield / 2, o?.await / 2, o.of / 2, o.default / 2 / 1, o.in / 2,
  o.return / 2, o.new / 2, o.class / 2, o.while(8) / 2, o?.while(6) / 2 / 1];
o.if++ / 2; r.push(o.if);
class K { if() { return 6 } #if = 40; get g() { return this.#if / 2 / 1 } }
r.push(new K().if() / 2, new K().g, ({ if: 8 }).if / 2);
for (var x of o.of / 2 ? [1] : []) r.push(x);
var p = {}; for (p.if of /a/g[Symbol.split]("bab")) r.push(p.if);
function* g() { yield /a/g; return o.yield / 4 }
var it = g(); r.push(it.next().value.flags, it.next().value);
function f() { return /x/.test("x") } r.push(f(), typeof /c/);
r.join(" ");
