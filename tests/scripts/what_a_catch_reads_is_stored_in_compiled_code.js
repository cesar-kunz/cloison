// A variable only a `catch` reads, in a function compiled (it has a
// loop): its store isn't dead in the `try`, where an exception from a
// callee goes on at the handler, in the interpreter, which reads it from
// the frame. (The JIT dropped `_this = this`: sass's dart2js code.) The
// expected value is V8's.
// expect: 224255520 caught M expected | caught M expected | caught M expected

function Obj(m) { this._map = m; }
Obj.prototype.wrap = function (callback, k) {
  var t3, acc = 0, _this = this, _s8_ = "expected";
  for (var j = 0; j < k; j++) acc += j;
  try {
    t3 = callback.call$0();
    return t3 + acc;
  } catch (exception2) {
    return "caught " + _this._map + " " + _s8_;
  }
};
function Cb(n) { this.n = n; }
Cb.prototype.call$0 = function () {
  if (this.n % 1000 === 999) null.boom;
  return this.n;
};
var o = new Obj("M"), out = [], s = 0;
for (var i = 0; i < 20000; i++) { var r = o.wrap(new Cb(i), 50); if (typeof r === "number") s += r; else if (out.length < 3) out.push(r); }
s + " " + out.join(" | ");
