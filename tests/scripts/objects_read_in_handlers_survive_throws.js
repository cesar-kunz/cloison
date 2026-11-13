// Objects only a `catch` or `finally` reads after a throw in its `try`,
// which the lowered code has no edge to: allocated by the exits there and
// before (an exit left `undefined` for them, #63), whether the throw is
// the code's, an inlined callee's or a callee's too big to inline, or
// after an exit before the `try`; nor left unspilled when an inlined
// call ending the `try` throws (past the call, the handler's reads
// weren't live: it crashed). The expected values are Node's.
// expect: 2500 4498500 2500 2500 2500 2500 2500 NaN 4496000

function P(x) { this.x = x; }
function thrower(i) { if (i == 2500) null.x; return i; }
function g1() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    if (i == 2500) {
      try { null.x; } catch (e) { s = o.x; }
    }
  }
  return s;
}
function g2() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    try { try { if (i == 2500) null.x; } finally { s += o.x; } } catch (e) {}
  }
  return s;
}
function g3() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(new P(i));
    try { if (i == 2500) null.x; } catch (e) { s += o.x.x; }
  }
  return s;
}
function g4() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    try { thrower(i); } catch (e) { s += o.x; }
  }
  return s;
}
function g5() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = { x: i };
    try { thrower(i); } catch (e) { s += o.x; }
  }
  return s;
}
function big(i) {
  var a = 0;
  for (var k = 0; k < 3; k++) {
    a += k * i; a ^= k; a += (i >> k) & 7; a -= k; a += i % 3; a *= 1; a += k; a -= 1; a += 2; a |= 0;
    a += k * i; a ^= k; a += (i >> k) & 7; a -= k; a += i % 3; a *= 1; a += k; a -= 1; a += 2; a |= 0;
  }
  if (i == 2500) throw new Error("x");
  return a;
}
function g6() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    try { big(i); } catch (e) { s += o.x; }
  }
  return s;
}
var vals = [];
for (var i = 0; i < 3000; i++) vals.push(i == 2500 ? "a" : i);
function g7() {
  var s = 0, t = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    t += vals[i] * 2;
    try { if (i == 2500) null.x; } catch (e) { s += o.x; }
  }
  return s + " " + t;
}
function g8() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    try { s += o.x; if (i == 2500) null.x; } catch (e) { s -= o.x; }
  }
  return s;
}
[g1(), g2(), g3(), g4(), g5(), g6(), g7(), g8()].join(' ');
