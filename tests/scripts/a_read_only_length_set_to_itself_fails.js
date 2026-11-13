// A store to an array's read-only `length` fails before converting the
// value, even of the value it has (cloison-fuzz BUGS.md #24).
// expect: TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError,TypeError 0 2 2

var r = [], calls = 0, a = Object.freeze([1, 2]), b = [1, 2];
Object.defineProperty(b, "length", { writable: false });
for (const v of [2, -1, 1.5, { valueOf() { calls++; return 2; } }]) {
  for (const o of [a, b]) {
    try { (function () { "use strict"; o.length = v; })(); r.push("ok"); } catch (e) { r.push(e.constructor.name); }
    o.length = v;
  }
}
r.join(",") + " " + calls + " " + a.length + " " + b.length;
