// A proxy's `ownKeys` result is read whole (CreateListFromArrayLike)
// before it is checked for duplicates (10.5.11 step 9).
// expect: TypeError 1

var read = 0, a = ["a", "a"];
Object.defineProperty(a, 2, { get() { read++; return "c"; } });
var r;
try { Reflect.ownKeys(new Proxy({}, { ownKeys() { return a; } })); r = "no throw"; }
catch (e) { r = e.constructor.name; }
r + " " + read;
