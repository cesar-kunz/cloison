// Another realm's `values`, `entries` or string `@@iterator` installed
// here makes iterators with that realm's prototypes (a builtin runs in
// its realm): their `next`, patched there, runs; the protector, which
// reads this realm's, mustn't take them for the builtins. (An array's
// `values` from there makes an iterator of that realm's too, whose
// `next` isn't the one patched here.)
// (`[c]` from a string done at once: `undefined`, which `join` leaves
// empty.)
// expect: [] 0 []  [1,2] 7 [3] ["q"] 4

var g = $262.createRealm().global, out = [];
g.Object.getPrototypeOf(new g.Set().values()).next = function () { return { done: true }; };
var sv = Set.prototype[Symbol.iterator];
Set.prototype[Symbol.iterator] = g.Set.prototype.values;
out.push(JSON.stringify([...new Set([1, 2])]));
Set.prototype[Symbol.iterator] = sv;
var m = new Map([[1, 2]]);
g.Object.getPrototypeOf(new g.Map().entries()).next = function () { return { done: true }; };
var me = Map.prototype[Symbol.iterator];
Map.prototype[Symbol.iterator] = g.Map.prototype.entries;
var n = 0; for (var x of m) n++; out.push(n);
Map.prototype[Symbol.iterator] = me;
var si = String.prototype[Symbol.iterator];
String.prototype[Symbol.iterator] = g.String.prototype[Symbol.iterator];
g.Object.getPrototypeOf(""[Symbol.iterator]()).next = function () { return { done: true }; };
out.push(JSON.stringify([..."xy"]));
var [c] = "zw"; out.push(c);
String.prototype[Symbol.iterator] = si;
var av = Array.prototype[Symbol.iterator], AIP = Object.getPrototypeOf([].values()), an = AIP.next;
AIP.next = function () { return { done: true }; };
Array.prototype[Symbol.iterator] = g.Array.prototype.values;
out.push(JSON.stringify([...[1, 2]]));
var [a] = [7]; out.push(a);
AIP.next = an; Array.prototype[Symbol.iterator] = av;
out.push(JSON.stringify([...new Set([3])]), JSON.stringify([..."q"]), [...[4]][0]);
out.join(" ")
