// A polymorphic call whose callee's load is dropped (the cases dispatch
// on the receiver's shape) keeps it when something between the load and
// the call may exit (`sym.name`, once a third shape comes): the
// interpreter would resume past the load and call what the callee's
// register held before.
// expect: 22000 27334 54666

function T1() { this.n = 0; } T1.prototype.add = function (k, v) { this.n += k.length; return true; };
function T2() { this.m = 0; this.n = 0; } T2.prototype.add = function (k, v) { this.n += 2 * k.length; return true; };
function Box(a, b) { this.a = a; this.b = b; }
function S1(name) { this.name = name; }
function S2(name) { this.x = 1; this.name = name; }
function S3(name) { this.y = 1; this.z = 2; this.name = name; }
function enter(box, pick, sym) {
  var t = pick ? box.a : box.b;
  if (pick === 2) { box.q0 = box.a.n + 0; box.r0 = box.b.n * 0; }
  if (pick === 3) { box.q1 = box.a.n + 1; box.r1 = box.b.n * 1; }
  if (pick === 4) { box.q2 = box.a.n + 2; box.r2 = box.b.n * 2; }
  if (pick === 5) { box.q3 = box.a.n + 3; box.r3 = box.b.n * 3; }
  if (pick === 6) { box.q4 = box.a.n + 4; box.r4 = box.b.n * 4; }
  if (pick === 7) { box.q5 = box.a.n + 5; box.r5 = box.b.n * 5; }
  if (pick === 8) { box.q6 = box.a.n + 6; box.r6 = box.b.n * 6; }
  if (pick === 9) { box.q7 = box.a.n + 7; box.r7 = box.b.n * 7; }
  if (pick === 10) { box.q8 = box.a.n + 8; box.r8 = box.b.n * 8; }
  if (pick === 11) { box.q9 = box.a.n + 9; box.r9 = box.b.n * 9; }
  if (pick === 12) { box.q10 = box.a.n + 10; box.r10 = box.b.n * 10; }
  if (pick === 13) { box.q11 = box.a.n + 11; box.r11 = box.b.n * 11; }

  if (!t.add(sym.name, sym)) return 0;
  return 1;
}
var box = new Box(new T1(), new T2()), s = 0;
for (var i = 0; i < 20000; i++) s += enter(box, i % 2 == 0, i % 3 ? new S1("ab") : new S2("abc"));
for (var i = 0; i < 2000; i++) s += enter(box, i % 2 == 0, new S3("abcd"));
s + " " + box.a.n + " " + box.b.n;
