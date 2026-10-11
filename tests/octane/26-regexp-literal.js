// Regular expression literals in compiled code: a new object at each
// evaluation (a `g` literal's `lastIndex` starts at 0 each time), in a
// loop, in a function inlined into it, with flags, and kept apart.
// for: regexp, pdfjs, typescript, mandreel
// expect: 90000;73333;0;0,5,1,1;a|b;true
function firstDigit(s) { return /\d/.exec(s)[0]; }
function countG(s) {
  var re = /a/g, n = 0;
  while (re.exec(s)) n++;
  return n;
}
function fresh() { return /x/g; }
var s = 0, t = 0, same = 0, keep = [];
for (var i = 0; i < 20000; i++) {
  s += +firstDigit("ab" + (i % 10) + "c");
  t += countG("banana" + (i % 3 ? "a" : ""));
  var a = fresh(), b = fresh();
  if (a === b) same++;
  a.lastIndex = 5;
  if (b.lastIndex !== 0) same += 100;
  if (i % 5000 === 0) keep.push(/K(\w)/i.exec("xk" + i)[1]);
  if (/^ab/m.test("x\nab") !== true) same += 1000;
}
[s, t, same, keep.join(","), /a|b/.source, /q/gim.global && /q/gim.multiline && /q/gim.ignoreCase].join(";");
