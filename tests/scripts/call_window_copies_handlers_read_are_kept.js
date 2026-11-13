// A variable copied into an inlined call's window, which the callee
// reads from its source instead, read after the call only by a `catch`:
// the copy's `Star` is kept, whether the throw is in the call or after it
// (its drop left the interpreter's register unwritten, `NaN`); and read
// after the call in a loop in a `catch`, which no edge reaches. The
// expected values are Node's, SpiderMonkey's, JSC's and QuickJS's.
// expect: 8997000 17989000 9007999 22495500

var I = 0;
function h(v) { if (I == 2500) null.x; return v; }
function k(v) { return v + 1; }
function g1() {
  var s = 0, a = 0, b = 0;
  for (var i = 0; i < 3000; i++) {
    b = i * 2; I = i; a = b;
    try { s += h(a); } catch (e) { s += a; }
    a = -1;
  }
  return s;
}
function g2() {
  var s = 0, a = 0, b = 0;
  for (var i = 0; i < 3000; i++) {
    b = i * 2; I = i; a = b;
    try { try { s += h(a); } finally { s += a; } } catch (e) {}
    a = -1;
  }
  return s;
}
function g3() {
  var s = 0, a = 0, b = 0;
  for (var i = 0; i < 3000; i++) {
    b = i * 2; I = i; a = b;
    try { s += k(a); s += h(1); } catch (e) { s += a; }
    a = -1;
  }
  return s;
}
function g4() {
  var s = 0, a = 0, b = 0;
  try { null.x; } catch (e) {
    for (var i = 0; i < 3000; i++) {
      b = i * 2; s += i; a = b;
      s += k(a);
      s += a;
    }
  }
  return s;
}
[g1(), g2(), g3(), g4()].join(' ');
