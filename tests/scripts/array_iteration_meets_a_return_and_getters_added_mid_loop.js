// A `return` added to `%ArrayIteratorPrototype%` while a for-of runs,
// which reads what is left with `this.next()`; a `return` getter on
// `%IteratorPrototype%` added mid-loop (read at each close); getters on
// `Array.prototype` indices met at holes, one defined mid-loop. As V8
// and SpiderMonkey.
// expect: 1,2,3,rest:4,5,6,get,ret:7,get,ret:9,8,10,11,12,i3,13,14

var AIP = Object.getPrototypeOf([].values()), IP = Object.getPrototypeOf(AIP), out = [];
// return added mid-loop, reading what is left from within it
for (var x of [1, 2, 3, 4]) {
  out.push(x);
  if (x === 2) AIP.return = function () { var rest = []; for (var r; !(r = this.next()).done;) rest.push(r.value); out.push("rest:" + rest.join("/")); return {}; };
  if (x === 3) break;
}
delete AIP.return;
// return as a getter on %IteratorPrototype%, added mid-loop
for (var x of [5, 6, 7]) {
  out.push(x);
  if (x === 5) Object.defineProperty(IP, "return", { get() { out.push("get"); var it = this; return function () { out.push("ret:" + it.next().value); return {}; }; }, configurable: true });
  if (x === 6) break;
}
var [p] = [8, 9]; out.push(p);
delete IP.return;
// getters on Array.prototype indices read during the loop
var h = [10, , 12, , 14];
Object.defineProperty(Array.prototype, 3, { get() { out.push("i3"); return 13; }, configurable: true });
for (var x of h) { out.push(x); if (x === 10) Object.defineProperty(Array.prototype, 1, { get() { return 11; }, configurable: true }); }
delete Array.prototype[1]; delete Array.prototype[3];
out.join()
