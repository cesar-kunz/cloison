// A native a compiled read runs that writes and then can't go on (the
// review of #161): `Object.assign` as a proxy's `get` trap writes the
// key's characters into the target (here through a setter that pushes),
// then reads the receiver's own getters: a native converting with a
// bytecode `toString`, a bytecode getter, one that throws. The redo
// must not write again (`sink.length` 2 per read, not 3). The expected
// value is SpiderMonkey's and JSC's.
// expect: {"2":3000} {"2":3000} {"2":3000} 1

var sink = { length: 1 };
Object.defineProperty(sink, "0", { set: Array.prototype.push, get() { return 0; } });
var p = new Proxy(sink, { get: Object.assign });
function receiver(kind) {
  var o = Object.create(p);
  Object.defineProperty(o, Symbol.toPrimitive, { value: undefined });
  if (kind == 0) {
    Object.defineProperty(o, "g", { get: String.prototype.trim, enumerable: true });
    o.toString = function () { return " ab "; };
  } else if (kind == 1) {
    Object.defineProperty(o, "g", { get() { return 1; }, enumerable: true });
  } else {
    Object.defineProperty(o, "g", { get() { throw 7; }, enumerable: true });
  }
  return o;
}
function read(kind) {
  var lengths = {};
  for (var i = 0; i < 3000; i++) {
    var o = receiver(kind);
    sink.length = 1;
    try { o.x; } catch (e) {}
    lengths[sink.length] = (lengths[sink.length] || 0) + 1;
  }
  return JSON.stringify(lengths);
}
[read(0), read(1), read(2), sink.g].join(" ");
