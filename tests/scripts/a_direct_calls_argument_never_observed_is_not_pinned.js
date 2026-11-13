// A direct call passing a value its caller never saw (`new C0(0, 0).z`,
// set only on a path not taken: `Bot`) to a parameter the callee uses
// as an int32: the callee doesn't keep it unboxed from entry (its entry
// asserted it entered int32), as V8, SpiderMonkey and JavaScriptCore.
// (cloison-fuzz's C04.)
// expect: 3000

var itb = { [Symbol.iterator]() { let k = 0; return { next() { return { value: k, done: k++ >= 8 }; } }; } };
var a0 = [];
var t0 = [0];
function C0(p, q) {
  this.y = 3;
  if (t0[0]) this.z = ({ x: 0, y: (q > a0[p & 15]) }).y;
}
function f0(d, p0 = -1) {
  if (d <= 0) throw 0;
  return ((((q3) => { for (let j = 0; j < (q3 & 7); j++) ; return 0; })(0)) & f0(d - 1, __shp[0].w, 0, 2));
}
function f3(d, p0) {
  a0[new C0(Array.from(itb, (v) => v * 2)[p0 & 7], ((q33) => { for (let j = 0; j < (q33 & 7); j++) ; return 0; })(p0)).y & 15] = (Array.from(itb, (v) => v * 2)[(5).toString(7).length & 7]);
  return f0(0, new C0(0, 0).z);
}
var n = 0;
for (var i = 0; i < 3000; i++) {
  ({ __proto__: null }).z;
  try { f3(2, i); } catch (e) { n++; }
}
n
