// `Iterator.concat` reads the `next` of the iterator an item's
// `@@iterator` returns, which only it refers to: a proxy whose handler's
// `get` getter collects stays alive to be stepped.
// expect: 1,2,1,2,1,2
function churn() { $262.gc(); var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i }); return junk.length; }
function fresh(k) {
  var t = { next() { k++; return k > 2 ? { done: true } : { value: k, done: false }; } };
  for (var i = 0; i < 50; i++) t["p" + i] = { x: i };
  return new Proxy(t, { get get() { churn(); return function (t, key) { return t[key]; }; } });
}
var out = [];
for (var round = 0; round < 3; round++)
  for (var v of Iterator.concat({ [Symbol.iterator]() { return fresh(0); } })) out.push(v);
out.join(",");
