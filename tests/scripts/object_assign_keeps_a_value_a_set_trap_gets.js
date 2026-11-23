// `Object.assign` stores each source's value on the target: a proxy
// here, whose `set` trap's lookup collects while a getter's new value
// is only in Rust.
// expect: t0|t1|t2
var handler = { get set() {
  var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i });
  return function (t, k, v) { seen.push(v.tag); return true; };
} };
var seen = [];
var target = new Proxy({}, handler);
for (var k = 0; k < 3; k++) Object.assign(target, { get a() { return { tag: "t" + k }; } });
seen.join("|");
