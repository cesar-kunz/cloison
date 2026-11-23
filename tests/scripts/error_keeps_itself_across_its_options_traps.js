// `new Error(message, options)` reads `cause` off its options, a proxy
// here: the new error stays alive through the collections its `has` and
// `get` traps cause.
// expect: m0c true|m1c true|m2c true
var opts = new Proxy({}, {
  has() { var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i }); return true; },
  get() { return "c"; }
});
var out = [];
for (var k = 0; k < 3; k++) { var e = new Error("m" + k, opts); out.push(e.message + e.cause + " " + (e instanceof Error)); }
out.join("|");
