// No scalar replacement of an object held across a call that isn't
// inlined: the callee's frame overwrote its fields' registers, and an
// exit after the call (here the arrow's throw) materialized the object
// from them (cloison-fuzz BUGS.md #13; reduced from a generated program,
// it crashed in the write barrier).
// expect: done

__fuel = { valueOf() { } };
a0 = [];
a2 = o3 = {};
function C0(q) {
  this.y = (String().replace(/]/, "d") - (q));
  if ((() => { for (let k1 = 0; k1 < 4; k1++) fs.push(() => (a2 = o3.z)); fs.reduce((u, f) => (f())); })()) (o3).length;
}
function f3(d, p0) {
  if (__fuel -= 0 || d <= 0) return;
  (a2[new C0(a0.length, p0).y] ? Number(f3(d - 1)).toFixed(0).length : parseInt(String(Object.assign({}, o0).y)));
}
[,];
for (i = 0; i < 50; i++) {
  try { f3(2, (() => { fs = []; })()); } catch (e) { }
}
"done";
