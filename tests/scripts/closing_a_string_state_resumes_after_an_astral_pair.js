// Closing a string's `IteratorOpen` state (a `break`, a short pattern)
// with a `return` added on `%StringIteratorPrototype%` makes an
// iterator at the state's index: past an astral pair, its two units. (As V8 and SpiderMonkey.)
// expect: 1,2,ret:b:false,ret:y:false,2,x,ret:😁:false,128512

var SIP = Object.getPrototypeOf(""[Symbol.iterator]()), out = [];
SIP.return = function () { var r = this.next(); out.push("ret:" + r.value + ":" + r.done); return {}; };
for (var ch of "a\u{1F600}bc") { out.push(ch.length); if (ch.length === 2) break; }
var [p, q] = "\u{1F600}xy"; out.push(p.length, q);
var [r1] = "\u{1F600}\u{1F601}"; out.push(r1.codePointAt(0));
delete SIP.return;
out.join()
