// A native that compiled code calls outside an operation (a getter, as
// `String.prototype.trim`) and that converts runs a `valueOf` or
// `toString` that writes, once: the read is redone for one it refused,
// in the interpreter, which runs it (#75, #129). The expected value is
// SpiderMonkey's, JSC's and QuickJS's.
// expect: ab/0,ab/0,ab/0

function mk(k) { var o = { 0: " ab ", length: 1 }; o[k] = Array.prototype.pop; return o; }
function f(n, k) {
  var r = "";
  for (var i = 0; i < n; i++) {
    var o = mk(k);
    Object.defineProperty(o, "t", { get: String.prototype.trim });
    r = o.t + "/" + o.length;
  }
  return r;
}
[f(1, "toString"), f(5000, "toString"), f(5000, Symbol.toPrimitive)].join(",");
