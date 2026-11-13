// A proxy on a chain the protector reads (here `%IteratorPrototype%`'s
// prototype) runs its traps for `return` as IteratorClose's GetMethod
// looks it up (§7.4.11): no bits while it is there.
// expect: return,return

var IP = Object.getPrototypeOf(Object.getPrototypeOf([].values())), old = Object.getPrototypeOf(IP), seen = [];
Object.setPrototypeOf(IP, new Proxy({}, { get(t, k) { seen.push(String(k)); } }));
for (const x of [1, 2]) break;
var [a] = [1, 2];
Object.setPrototypeOf(IP, old);
for (const x of [1, 2]) break;
seen.join()
