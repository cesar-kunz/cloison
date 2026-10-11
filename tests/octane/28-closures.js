// Closures created in compiled code: a new function at each evaluation
// with its own `prototype`, taking upvalues from the function creating
// it (not its registers: such a function isn't compiled), in a loop and
// in a function inlined into it.
// for: earley-boyer, gbemu, pdfjs, code-load, typescript
// expect: 200190000|20000|20000|15|function|true
function outer(k) {
  // `k` is captured: `outer` runs in the interpreter; `middle` reads it
  // as an upvalue, and creates closures that take it from its own.
  return function middle(x) {
    var f = function () { return k + x; };
    return f;
  };
}
function plain(i) { return function () { return 7; }; }
function adder(n) { var g = outer(n); return g; }
var mid = outer(3), s = 0, distinct = 0, protos = 0, prev = null;
for (var i = 0; i < 20000; i++) {
  var f = mid(i);
  s += f();
  var p = plain(i);
  if (p !== prev) distinct++;
  if (p.prototype && p.prototype.constructor === p) protos++;
  prev = p;
  s += p();
}
var h = adder(10)(5);
[s, distinct, protos, h(), typeof h, h.prototype.constructor === h].join("|");
