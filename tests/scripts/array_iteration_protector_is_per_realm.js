// The iteration protector's bits (`symbol::protector`) are a realm's:
// a realm's prototypes are swapped in on entering it, the epoch not
// bumped. After a `ShadowRealm` iterated, the main realm's patched
// `next` and `@@iterator` run; and a patched one in the main realm
// leaves the other realm's arrays to its builtins.
// expect: 4,6,14,11

var r = new ShadowRealm(), out = [];
r.evaluate(`for (const x of [1]) {}; 0`);
var AIP = Object.getPrototypeOf([].values()), on = AIP.next, c = 0;
AIP.next = function () { c++; return on.call(this); };
for (const x of [1, 2]) {}
var [p] = [3, 4];
out.push(c, r.evaluate(`var n = 0; for (const x of [1, 2, 3]) n += x; n`));
AIP.next = on;
var vi = Array.prototype[Symbol.iterator];
Array.prototype[Symbol.iterator] = function () { c += 10; return vi.call(this); };
for (const x of [1]) {}
out.push(c, r.evaluate(`var [a, b] = [5, 6]; a + b`));
Array.prototype[Symbol.iterator] = vi;
out.join()
