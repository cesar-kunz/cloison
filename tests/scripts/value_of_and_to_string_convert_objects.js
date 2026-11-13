// ToPrimitive calls bytecode `valueOf` and `toString` (`interp::
// to_primitive_hint`): in the interpreter's slow paths, in natives
// (`Math.max`), and for compiled code by leaving it for the interpreter
// (`jit::RT_REDO`), once each; what one throws propagates; neither
// giving a primitive is a TypeError. The expected value is Node's.
// expect: 12,5,-6,2,1,-4,5,16,true,true,true,4x,x4,V4,14,14,71,7,2,caught boom,caught2 boom,true,233989,41

var calls = 0;
function V(n) { this.n = n; }
V.prototype.valueOf = function () { calls++; return this.n; };
V.prototype.toString = function () { return "V" + this.n; };
var a = new V(4), b = new V(10);
var r = [];
r.push(a * 3, a + 1, a - b, a / 2, a % 3, -a, a | 1, a << 2, a > 3, a < b, a == 4, a + "x", "x" + a, String(a), a + b);
var o2 = { toString: function () { return "7"; } };
r.push(o2 * 2, o2 + 1, Math.max(a, o2), [1, 2, 3][o2 > 5 ? 1 : 0]);
var thrower = { valueOf: function () { throw new Error("boom"); } };
try { r.push(thrower + 1); } catch (e) { r.push("caught " + e.message); }
try { r.push(Math.floor(thrower)); } catch (e) { r.push("caught2 " + e.message); }
var bad = { valueOf: function () { return {}; }, toString: function () { return {}; } };
try { r.push(bad * 1); } catch (e) { r.push(e instanceof TypeError); }
// In a hot loop: compiled code meeting objects.
function sum(xs) { var s = 0; for (var i = 0; i < xs.length; i++) s = s + xs[i] * 2 - 1; return s; }
var xs = []; for (var i = 0; i < 2000; i++) xs.push(i % 7);
var t = 0; for (var k = 0; k < 20; k++) t += sum(xs);
xs[1500] = new V(100); t += sum(xs);
function lt(xs) { var c = 0; for (var i = 0; i < xs.length; i++) if (xs[i] < 3.5) c++; return c; }
for (var k = 0; k < 20; k++) t += lt(xs);
xs[10] = new V(1); t += lt(xs);
r.push(t, calls);
r.join(",");
