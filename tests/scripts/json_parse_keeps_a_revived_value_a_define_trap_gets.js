// `JSON.parse` defines each revived value on its holder: a proxy here
// (a sibling's reviver put it there), whose `defineProperty` trap's
// lookup collects while the reviver's new object is only in the
// descriptor being passed.
// expect: t0|t1|t2
var handler = {
  ownKeys() { return ["x"]; },
  getOwnPropertyDescriptor() { return { value: 1, enumerable: true, configurable: true, writable: true }; },
  get() { return 1; },
  get defineProperty() {
    var junk = []; for (var i = 0; i < 20000; i++) junk.push({ i });
    return function (t, k, d) { seen.push(d.value.tag); return true; };
  }
};
var seen = [];
for (var k = 0; k < 3; k++)
  JSON.parse('{"a":1,"b":2}', function (key, val) {
    if (key === "a") this.b = new Proxy({}, handler);
    if (key === "x") return { tag: "t" + k };
    return val;
  });
seen.join("|");
