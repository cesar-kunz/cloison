// `arguments` in compiled code: in a function's own frame (from its call)
// and in one inlined into its caller (from the call's registers): no
// arguments, fewer than the parameters, more; through `f.call`, `new`,
// `f.apply`; read by index and length, kept, and passed on.
// for: earley-boyer, code-load, pdfjs
// expect: 2280825000|2,,2,,2,|2|2
function list() {
  var res = null, a = arguments;
  for (var i = a.length - 1; i >= 0; i--) res = { car: a[i], cdr: res };
  return res;
}
function len(l) { var n = 0; while (l !== null) { n++; l = l.cdr; } return n; }
function sum2(x, y) { var s = 0; for (var i = 0; i < arguments.length; i++) s += arguments[i]; return s + (y === undefined ? 1000 : 0); }
function keep() { return arguments; }
function Pt() { this.n = arguments.length; this.first = arguments[0]; }
function count() { return arguments.length; }
var s = 0, keeps = [];
for (var i = 0; i < 30000; i++) {
  s += len(list()) + len(list(i)) + len(list(i, i, i));
  s += sum2(i) + sum2(i, 1) + sum2(i, 1, 2, 3);
  s += sum2.call(null, i, 2, 3) + sum2.apply(null, [i, 5]);
  var p = new Pt(i, 2, 3);
  s += p.n + (p.first === i ? 1 : 0);
  s += count() + count(1, 2, 3, 4, 5);
  if (i % 10000 === 0) keeps.push(keep(i, "x").length, keep()[0]);
}
[s, keeps.join(","), list(1, 2).cdr.car, count.apply(null, [1, 2])].join("|");
