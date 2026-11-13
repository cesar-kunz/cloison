// A compound assignment's converted object key outlives a collection in
// its right side: `Vm::key_cache` kept the key string `GetKeyed` made for
// the `SetKeyed` to reuse, but no collection marked it, so the store went
// to whatever string took its freed cell ("s396"), or crashed.
// expect-json: "20 "

var o = {}, n = 0, junk;
function rhs() {
  $262.gc(); $262.gc();
  for (var i = 0; i < 20000; i++) junk = "s" + i;
  return 1;
}
function f() {
  var k = { toString() { return "key" + (n++); } };
  o[k] += rhs();
}
for (var r = 0; r < 20; r++) f();
var ks = Object.keys(o), bad = [];
for (var i = 0; i < 20; i++) if (ks[i] !== "key" + i) bad.push(ks[i]);
ks.length + " " + bad.join(" ");
