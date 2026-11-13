// A key a proxy's `ownKeys` trap lists but its `getOwnPropertyDescriptor`
// trap says isn't there doesn't hide a prototype's: only a key with a
// descriptor joins the visited keys (%ForInIteratorPrototype%.next,
// 14.7.5.10.2.1).
// expect: u,v

var t = Object.create({ u: 1, v: 2 });
var p = new Proxy(t, { ownKeys() { return ["u"]; }, getOwnPropertyDescriptor() { return undefined; } });
var s = []; for (var k in p) s.push(k);
s.join(",");
