// `Math.sqrt` and `Math.pow` compiled inline (`Insn::native` 6 and 7):
// the argument in `acc` (live into the call), `pow`'s base read from
// its own register; strings and objects leave for the interpreter. The
// expected value is Node's.
// expect: 30501185.765135095,10743.993963615587,1009.8377223398317,NaN,9,NaN

var o = { g: 2 };
function f(o, y) {
  var a = Math.pow(10, o.g + 1);
  var b = Math.pow(Math.max(y, 0), o.g);
  var c = 0.5 * Math.pow(y, 0.5);
  var d = (-y) - Math.sqrt(y * 2.5);
  return a + b + c + d;
}
function V(x, y) { this.x = x; this.y = y; }
V.prototype.magnitude = function () { return Math.sqrt(this.x * this.x + this.y * this.y); };
var s = 0, m = 0;
for (var i = 0; i < 30000; i++) { s += f(o, i % 9); m += 1 / new V(i % 7, 1.5).magnitude(); }
[s, m, f(o, "4"), Math.sqrt("x"), Math.pow({ valueOf: function () { return 3; } }, 2), Math.pow(-8, 1 / 3)].join();
