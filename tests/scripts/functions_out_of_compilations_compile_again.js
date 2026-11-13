// A callee typed anew by objects of many shapes, called from
// interpreted and compiled code: out of compilations (all the sooner
// under `CLOISON_JIT_COMPILES=4`), its start is retried, compiled
// once more for any types after its calls refused (`regrant`), and
// computes what V8, SpiderMonkey, JavaScriptCore and QuickJS do.
// expect: 554554 1012101

function visit(n, d) {
  var t = 0;
  for (var j = 0; j < 2; j++) t = t + n.k;
  if (d & 1) t = t + n.v;
  if (d & 2) t = t * 2 + n.w;
  return t + (n["p" + (d % 7)] | 0);
}
var kinds = [];
for (var i = 0; i < 300; i++) {
  var o = {}; o["q" + i] = i;
  if (i & 1) o.v = 0.5 * i;
  o.k = i & 3 ? i : "s" + i; o.w = i & 4 ? i : -i; o["p" + (i % 7)] = i;
  kinds.push(o);
}
var s = 0, z = "";
for (var r = 0; r < 200; r++)
  for (var i = 0; i < 300; i++) {
    var x = visit(kinds[(i * 7 + r) % 300], i + r);
    if (typeof x == "number") s = (s + (x === x ? x : 1)) % 1000003; else z = x.length + z.slice(0, 5);
  }
s + " " + z
