// A field loaded again is the first load's value (`jit::reuse_field_loads`)
// only while nothing may have changed it: not past a store to it through
// another register (`alias(a, a)`), a keyed store with its name, a call, a
// `valueOf` (which compiled code leaves to the interpreter), a setter
// (`join(set, set)`), a store on one of two joining paths, the receiver
// reassigned on one, the receiver itself overwritten by the load (`x =
// x.next; x = x.next`), or a call's argument copy an inlined callee
// doesn't read (`window`: its `Star` emits nothing); and a transition keeps
// the old fields. The expected value is what V8 and SpiderMonkey print.
// expect: 6000 20994 42000 9004 9006000 60000 45000 1349895000 4504500 9001503 19500 2500 2500 4516500 4498500

var N = 3000;
function P(x, y) { this.x = x; this.y = y; }
function L(v, n) { this.v = v; this.next = n; }
function alias(a, b) { var s = 0; for (var i = 0; i < N; i++) { var t = a.x; b.x = i & 7; s += t + a.x; } return s; }
function keyed(a, k) { var s = 0; for (var i = 0; i < N; i++) { var t = a.x; a[k] = i & 3; s += t + a.x; } return s; }
function bump(o) { o.x++; }
function call(a) { var s = 0; for (var i = 0; i < N; i++) { var t = a.x; bump(a); s += t + a.x; } return s; }
function conv(a, w) { var s = 0; for (var i = 0; i < N; i++) { var t = a.x; var u = t * w; s += u + a.x; } return s; }
function join(a, b) { var s = 0; for (var i = 0; i < N; i++) { var t = a.x; if (i & 1) { s += 1; } else { b.x = i; a.y = i; } s += t + a.x + a.y; } return s; }
function reassign(a, b) { var s = 0; for (var i = 0; i < N; i++) { var o = a; var t = o.x; if (i & 1) o = b; s += t + o.x; } return s; }
function skip2(h) { var s = 0, x = h; while (x !== null && x.next !== null) { s += x.v; x = x.next; x = x.next; } return s; }
function trans() { var s = 0; for (var i = 0; i < N; i++) { var o = new P(1, 2); var t = o.x; o.z = 5; o.x = i; s += t + o.x + o.z; } return s; }
function Sc() { this.pos = 0; }
Sc.prototype.peek = function () { return this.pos; };
function window(p) { var r = 0; for (var i = 0; i < N; i++) { var sc = p.scanner; r += sc.peek(); sc = i; r += p.scanner.peek() + sc; } return r; }
var h = null; for (var i = 0; i < 100; i++) h = new L(i, h);
var a = new P(1, 2), tgt = new P(5, 6), flag = false;
var w = { valueOf: function () { if (flag) tgt.x += 100; return 2; } };
var set = new P(1, 2); Object.defineProperty(set, "y", { set: function (v) { this.x = v; }, get: function () { return 3; } });
var out = [alias(a, new P(3, 4)), alias(a, a), keyed(a, "y"), keyed(a, "x"), call(new P(1, 1)), conv(tgt, 3), conv(tgt, w)];
flag = true;
out.push(conv(tgt, w), join(new P(1, 2), new P(0, 0)), join(set, set), reassign(new P(1, 0), new P(10, 0)), skip2(h), skip2(h), trans());
out.push(window({ scanner: new Sc() }));
out.join(" ");
