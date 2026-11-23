// `split` reads the species off `constructor`: a getter's new object
// with a proxy on its prototype chain, the receiver of the trap, stays
// alive through the collection the trap's lookup causes.
// expect: t0|t1|t2
var handler = { get get() {
  var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i });
  return function (t, k, r) { seen.push(r.tag); return undefined; };
} };
var seen = [];
for (var k = 0; k < 3; k++) {
  var re = /b/;
  var tag = "t" + k;
  Object.defineProperty(re, "constructor", { get() { var p = new Proxy({}, handler); return Object.create(p, { tag: { value: tag } }); } });
  "abc".split(re);
}
seen.join("|");
