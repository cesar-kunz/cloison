// A `return` added to `%StringIteratorPrototype%` while a for-of runs,
// which reads what is left with `this.next()`; then a pattern's close;
// then a `return` getter giving `undefined` (called, nothing closed). (As V8 and SpiderMonkey.)
// expect: a,b,😀,rest:cd,rest:q,2,p,get

var SIP = Object.getPrototypeOf(""[Symbol.iterator]()), out = [];
for (var c of "ab\u{1F600}cd") {
  out.push(c);
  if (c === "b") SIP.return = function () { var rest = ""; for (var r; !(r = this.next()).done;) rest += r.value; out.push("rest:" + rest); return {}; };
  if (c === "\u{1F600}") break;
}
var [x, y] = "\u{1F600}pq"; out.push(x.length, y);
Object.defineProperty(SIP, "return", { get() { out.push("get"); return undefined; }, configurable: true });
for (var c of "uv") break;
delete SIP.return;
out.join()
