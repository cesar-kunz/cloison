// A proxy of an array is an array to `concat`, `flat`, species creation
// and `JSON`'s replacer and reviver (IsArray), a revoked one a TypeError.
// expect: 2 [1,2,3] {"a":1,"c":3} [2,[4,6]] true TypeError

var r = [[].concat(new Proxy([1, 2], {})).length, JSON.stringify([1, new Proxy([2, 3], {})].flat()),
  JSON.stringify({ a: 1, b: 2, c: 3 }, new Proxy(["a", "c"], {})),
  JSON.stringify(JSON.parse("[1,[2,3]]", function (k, v) { return Array.isArray(v) ? new Proxy(v, {}) : v * 2; }))];
class A extends Array {}
r.push(Array.prototype.map.call(new Proxy(new A(1, 2, 3), {}), x => x).constructor === A);
var rev = Proxy.revocable([], {}); rev.revoke();
try { [].concat(rev.proxy); r.push("no"); } catch (e) { r.push(e.name); }
r.join(" ");
