// More ways a replaced object reaches a handler that reads it (each one
// failed before #63's fix): a loop in the `try`, entered at its header
// with the object held; a `catch` that throws again, through a `finally`,
// to an outer `catch` (which caught the `TypeError` and gave a wrong sum);
// a `valueOf` throwing in arithmetic; the allocation itself in the `try`;
// a `finally` passing the exception on to the `catch` reading it. The
// expected values are Node's, SpiderMonkey's, JSC's and QuickJS's.
// expect: 2995 9002000 4498500 2500 2503000

function P(x) { this.x = x; }
function g1(n) {
  var s = 0;
  try {
    for (var i = 0; i < n; i++) { var o = new P(i); if (i == n - 5) null.x; }
  } catch (e) { s = o.x; }
  return s;
}
function g2() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i), q = new P(-i);
    try {
      try { if (i == 2500) null.x; } catch (e) { s += q.x; throw e; } finally { s += o.x * 2; }
    } catch (e2) { s += o.x * 3; }
  }
  return s;
}
var vals = [];
for (var i = 0; i < 3000; i++) vals.push(i == 2500 ? { valueOf() { throw 1; } } : i);
function g3() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    try { s += vals[i] * 1; } catch (e) { s += o.x; }
  }
  return s;
}
function g4() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    try { var o = new P(i); if (i == 2500) null.x; o = new P(-1); } catch (e) { s += o.x; }
  }
  return s;
}
function g5() {
  var s = 0;
  for (var i = 0; i < 3000; i++) {
    var o = new P(i);
    try { try { if (i == 2500) null.x; } finally { s += 1; } } catch (e) { s += o.x * 1000; }
  }
  return s;
}
[g1(3000), g2(), g3(), g4(), g5()].join(' ');
