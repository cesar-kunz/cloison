// An iterator helper reads `done`, then `value`, from the result its
// underlying `next()` returns, which only it refers to: a proxy whose
// handler's `get` getter collects stays alive through both reads.
// expect: object|object|object
function churn() { $262.gc(); var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i }); return junk.length; }
var handler = { get get() { churn(); return function (t, k, r) { return k === "done" ? false : k === "value" ? r : t[k]; }; } };
var n = 0;
var src = { next() {
  n++;
  if (n > 3) return { done: true };
  var t = { tag: "t" + n };
  for (var i = 0; i < 50; i++) t["p" + i] = { x: i };
  return new Proxy(t, handler);
} };
var out = [];
for (var v of Iterator.prototype.map.call(src, x => x)) out.push(typeof v);
out.join("|");
