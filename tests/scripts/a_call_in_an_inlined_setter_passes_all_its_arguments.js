// A setter inlined into its caller, calling a method that is inlined too:
// the method's arguments are all moved, though the setter's own value was
// in place already (`preset_args`, for the setter's frame only). The
// method read its first argument from a register nothing wrote.
// expect: 80000 0 80000 a0b04

function keyOf(k) { return k; }
class E {
  constructor() { this.log = ""; }
  m(key, v) {
    if (key === "class") return;
    var k = keyOf(key);
    this.log += k + v;
  }
  set a(v) { this.m("a", v); }
  set b(v) { this.m("b", v); }
}
function f(o, n) {
  var t = 0;
  for (var i = 0; i < n; i++) {
    o.a = i & 3;
    o.b = i & 1;
    if (i % 1000 == 999) { t += o.log.length; o.log = ""; }
  }
  return t + " " + o.log.slice(-8) + o.log.length;
}
f(new E(), 20000) + " " + f(new E(), 20001);
