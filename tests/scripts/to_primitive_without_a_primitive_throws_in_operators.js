// ToPrimitive of an object whose `valueOf` and `toString` give no
// primitive (not callable, missing, or returning objects) throws a
// TypeError in every operator, also in an operation's runtime call
// that has no frame top, not giving "[object Object]" (#130); compiled
// code's runtime calls leave what one threw (a symbol's too) to the
// interpreter's redo. The expected value is SpiderMonkey's and JSC's.
// expect: 400,400,300,300,400,400,400,400,400,400,400,0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,s,1,2,3,[object Object]

var bad = [{ valueOf: 1, toString: 1 }, Object.create(null),
  { valueOf() { return {}; }, toString() { return {}; } }, Symbol()];
var ops = [(a) => a + "", (a) => 1 + a, (a) => a == 1, (a) => a != "x", (a) => a < 1,
  (a) => 1 >= a, (a) => a * 1, (a) => a - 1, (a) => a | 0, (a) => `${a}`, (a) => +a];
var r = [];
for (var f of ops) {
  var n = 0;
  for (var o of bad) {
    for (var i = 0; i < 700; i++) {
      try { f(i % 7 ? 5 : o); } catch (e) { if (i % 7 == 0 && e instanceof TypeError) n++; }
    }
  }
  r.push(n);
}
function sum(xs) { var s = 0; for (var i = 0; i < xs.length; i++) s = s + xs[i]; return s; }
var xs = []; for (var i = 0; i < 1000; i++) xs.push(i);
for (var j = 0; j < 20; j++) {
  sum(xs);
  var ys = xs.slice(); ys[500] = bad[j % 4];
  try { sum(ys); } catch (e) { if (e instanceof TypeError) r.push(j); }
}
r.push({ valueOf: 1, toString() { return "s"; } } + "", [1, [2, 3]] + "", {} + "");
r.join();
