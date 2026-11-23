// `RegExp.prototype[Symbol.match]` reads `0` off each `exec` result:
// one `exec` just made, with a proxy on its prototype chain, is the
// receiver of the trap, and stays alive through the collection the
// trap's lookup causes.
// expect: t0,t0|t1,t1|t2,t2
var handler = { get get() {
  var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i });
  return function (t, k, r) { return k === "0" ? r.tag : undefined; };
} };
var p = new Proxy({}, handler);
var out = [];
for (var k = 0; k < 3; k++) {
  var n = 0, tag = "t" + k;
  var re = { flags: "g", set lastIndex(v) {}, get lastIndex() { return 0; },
    exec() { return n++ < 2 ? Object.create(p, { tag: { value: tag } }) : null; } };
  out.push(RegExp.prototype[Symbol.match].call(re, "x").join(","));
}
out.join("|");
