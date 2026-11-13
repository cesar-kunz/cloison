// An object loaded from a field or an element typed as its shape
// (`Ty::Ext`) that leaves the shape (a property deleted or made an
// accessor) drops the code that took it to have exactly that shape, and
// the shape is not taken as exact again (`object::Shape::left`).
// expect: NaN,400,400,700,91,250000

function C() { this.a = 1; this.b = 2; }
var arr = [new C(), new C()], holder = { c: new C() };
function ra() { var s = 0; for (var i = 0; i < 100; i++) s += arr[i & 1].a; return s; }
function rf() { var s = 0; for (var i = 0; i < 100; i++) s += holder.c.a; return s; }
function rs() { var s = ""; for (var i = 0; i < 2; i++) s += arr[i & 1].a; return s; }
function rl(n) {
  var s = 0;
  for (var i = 0; i < 100000; i++) { if (i == n) { delete arr[1].a; arr[1].a = 7; } s += arr[i & 1].a; }
  return s;
}
function main() {
  var r = [];
  for (var k = 0; k < 300; k++) { ra(); rf(); }
  delete arr[0].a;
  r.push(ra());
  arr[0].a = 7;
  r.push(ra());
  for (var k = 0; k < 300; k++) ra();
  r.push(ra());
  delete holder.c.a; holder.c.a = 7;
  r.push(rf());
  arr = [new C(), new C()];
  for (var k = 0; k < 3000; k++) rs();
  Object.defineProperty(arr[0], "a", { get: function () { return 9; } });
  r.push(rs());
  arr = [new C(), new C()];
  rl(-1);
  r.push(rl(50000));
  return r.join();
}
main();
