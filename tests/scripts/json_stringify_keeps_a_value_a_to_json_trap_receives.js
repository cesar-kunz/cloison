// `JSON.stringify` reads `toJSON` off each object it serializes: one a
// getter just made, with a proxy on its prototype chain, is the
// receiver of its trap, and stays alive through the collection the
// trap's lookup causes.
// expect: {"a":{"x":1}}|{"a":{"x":1}}|{"a":{"x":1}}
var handler = { get get() {
  var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i });
  return function () { return undefined; };
} };
var p = new Proxy({}, handler);
var o = { get a() { return Object.create(p, { x: { value: 1, enumerable: true } }); } };
var out = [];
for (var k = 0; k < 3; k++) out.push(JSON.stringify(o));
out.join("|");
