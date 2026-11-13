// Destructured parameters, `**`, `??`, logical assignment, optional
// chains, numeric separators, optional catch bindings and object spread.
// expect: 5 6 3 | 1024 8 4 0.5 512 NaN NaN | 9 16 | 9 | 1 0 u false  | 1 0 | 5 3 7 0 | set 1 r | 1 | 8,1 1 | fn | undefined undefined 1 1 true undefined 10 | undefined true undefined 0 | undefined true true {} | 0.5 | 1,2 undefined | a10bundefinedc 3 | 1000000 170 10.5 10000000000 63 | caught | {"0":"h","1":"i","a":1,"b":2,"c":3} | 1 {"b":2,"c":3} | 4 {"z":5} | 1 1 | 0 1

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
// destructured params
function f1({b=2}={}, [c]=[3]) { return b + c; }
function f2(a, {b}) { var b2 = b; return a + b2; }
log(f1(), f1({b:5},[1]), f2(1, {b:2}));
// **
var x = 2; x **= 3; log(2**10, x, (-2)**2, 2**-1, 2**3**2, Math.pow(1, NaN), 1 ** NaN);
var o = {v: 3}; o.v **= 2; var arr = [2]; arr[0] **= 4; log(o.v, arr[0]);
log({valueOf(){ return 3 }} ** 2);
// ??
log(null ?? 1, 0 ?? 2, undefined ?? "u", false ?? true, "" ?? "e");
var calls = 0; function side() { calls++; return 5; }
log(1 ?? side(), calls);
// logical assignment
var a = null; a ??= 5; var b = 0; b ||= 3; var c = 1; c &&= 7; var d = 0; d &&= 9;
log(a, b, c, d);
var obj = {p: null, q: 1}; obj.p ??= "set"; obj.q ??= "no"; obj.r ||= "r";
log(obj.p, obj.q, obj.r);
var cnt = 0; var holder = {get g(){ cnt++; return 1 }, set g(v){ cnt += 100 }}; holder.g ||= 2; log(cnt);
var ks = 0; var ar2 = [0, 1]; ar2[ks++] ||= 8; log(ar2.join(), ks);
var fn = null; fn ??= function () {}; log(fn.name);
// ?.
var n = null, u, deep = {a: {b: {c: 1}}, f() { return this === deep; }, arr: [10]};
log(n?.x, u?.x.y.z, deep?.a?.b?.c, deep.a?.["b"].c, deep.f?.(), deep.g?.(), deep.arr?.[0]);
log(n?.(), deep?.f(), typeof u?.[side()], calls);
log((n?.a)?.b, delete n?.x, delete deep?.a.b, JSON.stringify(deep.a));
log(true?.5:1);
var spreadArgs = [1,2]; var mm = {m(...xs){ return this === mm && xs.join(); }}; log(mm.m?.(...spreadArgs), mm.z?.(...spreadArgs));
log(`a${deep.arr?.[0]}b${n?.x}c`, `${mm?.m(3)}`);
// numeric separators
log(1_000_000, 0b1010_1010, 1_0.5_0, 1e1_0, 0o7_7);
// optional catch
try { throw 1 } catch { log("caught") }
// object spread / rest
var src = {a: 1, b: 2}; var sp = {...src, c: 3, ...null, ...undefined, ..."hi"};
log(JSON.stringify(sp));
var {a: aa, ...rest} = {a: 1, b: 2, c: 3}; log(aa, JSON.stringify(rest));
var r2; ({a: aa, ...r2} = {a: 4, z: 5}); log(aa, JSON.stringify(r2));
var getterCalls = 0; var gs = {...{get g() { getterCalls++; return 1; }}}; log(getterCalls, Object.getOwnPropertyDescriptor(gs, "g").value);
var protoSetter = 0; Object.defineProperty(Object.prototype, "ps", {set(v) { protoSetter++; }, configurable: true});
var sp2 = {...{ps: 1}}; log(protoSetter, sp2.ps); delete Object.prototype.ps;
out.join(" | ");
