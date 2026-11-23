// `String.raw` of a primitive reads `raw` off its new wrapper: with a
// proxy on `String.prototype`'s chain, the wrapper is the receiver of
// its trap, and stays alive through the collection the trap's lookup
// causes.
// expect: ab0|ab1|ab2
var handler = { get get() {
  var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i });
  return function (t, k, r) { return k === "raw" ? { 0: String(r), length: 1 } : undefined; };
} };
Object.setPrototypeOf(String.prototype, new Proxy({}, handler));
var out = [];
for (var k = 0; k < 3; k++) out.push(String.raw("ab" + k));
out.join("|");
