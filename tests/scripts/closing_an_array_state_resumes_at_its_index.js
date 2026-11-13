// With a `return` added to `%ArrayIteratorPrototype%`, closing an
// array's `IteratorOpen` state (a `break`, a pattern shorter than the
// array) makes an iterator at the state's index for it: its
// `this.next()` goes on from there (V8 and SpiderMonkey agree).
// expect: 1,ret:2:false,ret:6:false,4,5,ret:undefined:true,8,ret:13:false,10,12

var AIP = Object.getPrototypeOf([].values()), out = [];
AIP.return = function () { var r = this.next(); out.push("ret:" + r.value + ":" + r.done); return {}; };
for (var x of [1, 2, 3]) { out.push(x); if (x === 1) break; }
var [a, b] = [4, 5, 6, 7]; out.push(a, b);
var [c] = [8]; out.push(c);
var [d, , e] = [10, 11, 12, 13]; out.push(d, e);
delete AIP.return;
out.join()
