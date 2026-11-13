// A store's inline cache can lead to a shape a full collection found
// dead (`Vm::sweep_shapes`, a zombie: its prototype null) while the
// shape it starts from lives: `P0`'s constructor, shared by several
// prototypes, keeps `P1`'s root shape (`Vm::ctor_protos`), while every
// `P1` object, and with them the shapes after `this.x = v`, dies. Live
// codes' transitions are live shapes. Without, `new P1(x)` had no
// prototype after such a collection, its `z` undefined, in about three
// runs in four (found by cloison-fuzz's hunt r20 as a nondeterministic
// result). Generated, reduced: `__miss` counts the `new P1(x).z` that
// didn't call the getter.
// expect: 0

function __str(v, d) {
  switch (typeof v) {
    case "number": return Object.is(v, -0) ? "-0" : "" + v;
    case "string": return JSON.stringify(v);
    case "undefined": case "boolean": return "" + v;
  }
  var ks = Object.keys(v);
  r = Object.getPrototypeOf(v) === null ? "N{" : "{";
  for (k = 0; k < ks.length && k < 8; k++) r += ks[k] + ":" + __str(v[ks[k]], d + 1) + ",";
  return r + "}";
}
function __name(e) {
}
function __mix(h, v) {
}
var __fuel = 300000;
function __inc(g) { return typeof g === "number" ? (g + 1) | 0 : 1; }
var __vo = { valueOf() { g0 = __inc(g0); return g0 & 7; } };
var __vt = { valueOf() { throw new TypeError("v"); } };
function* __gen(n) { for (let k = 0; k < n && __fuel-- > 0; k++) yield k * 3; return n; }
var __cl = [], __pend = 0, __ing = false;
var __dv = new DataView(new ArrayBuffer(16));
var __itb = { [Symbol.iterator]() { let k = 0; return { next() { return { value: k * 2, done: k++ >= 8 }; }, return() { g2 = __inc(g2); return {}; } }; } };
var g0 = -3;
var g1 = -2;
var g2 = 69;
var s0 = "-3.5";
var s1 = "";
var s2 = "aab";
var a0 = new Array(6);
var a1 = new Array(6);
var a2 = [];
function C0(p, q) {
}
class P0 {
  constructor(v) { this.x = v; this.y = 1; }
  get z() { __zc++; return this.x * 2; }
}
class P1 extends P0 {
}
class P2 extends P1 {
}
o2 = Object.create(null);
async function f0(d, p0, p1 = 65536) {
}
function f1(d, p0) {
  while (w4++ < 5 && __fuel-- > 0) {
    try {
    } finally {
    }
  }
}
function f2(d, p0, p1) {
}
function f3(d, p0, p1, p2) {
  for (const v6 of a2.slice(0, 8)) {
    try {
    } finally {
    }
  }
  switch ((((() => { let u = 0; for (const v10 of __gen(p0 & 7)) u = (u + v10) | 0; return u; })() % (o2 instanceof P1 ? 1 : 0))) & 3) {
    case 0:
      for (const v11 of __itb) {
      }
      for (const v12 of __itb) {
      }
  }
  for (const v14 of __itb) {
    for (let k15 = 0; k15 < (((/a+b?|\d+/.test(String(p1)) ? 1 : 0) | (a1.length < 64 ? a1.push(65535) : -4)) & 15) && __fuel-- > 0; k15++) {
      let l16 = __dv.getUint8((() => { let u = 0; for (const v17 of __gen(({ x: v14, y: a0.length }).x & 7)) u = (u + v17) | 0; return u; })() & 7, false);
      for (const v18 of a1.slice(0, 8)) {
        g0 = (g0 + eval("l16 * 2")) | 0;
      }
    }
  }
}
var __acc = 0, __samples = [], __zc = 0, __miss = 0;
var __alt = [Infinity, undefined, __vt, __vo];
for (var i = 710; i < 3000; i++) {
  var x = __alt[i & 3];
  var __z0 = __zc, __a; try { __a = a0.findLast((v) => v != new P1(x).z); } catch (e) { __z0 = -1; } if (__zc === __z0) __miss++;
  try { __pend++; Promise.resolve(f3(3, x, __a, i & 7)).then((r) => { __acc = __mix(__acc, r); __samples.push(__str(r)); __pend--; }, (e) => { __acc = __mix(__acc, "E:" + __name(e)); __pend--; }); }
  catch (e) { __pend--; __acc = __mix(__acc, "E:" + __name(e)); if (i % 97 === 0) __samples.push("E:" + __name(e)); }
}

__miss;
