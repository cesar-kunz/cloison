// A store adding an element calls a setter, or fails on a read-only
// element, on the prototype chain (`push` too), once one exists there;
// code compiled before then stops storing elements directly. Literals
// and builtins that define elements don't call setters.
// expect: 4:0,1,P,3 | 4:0,1,P,3 |  | true | 2,4,6 | 4,5,6 | 0,1,2 | false | 4 | 5004 | 0,1,2,3

var out = [];
function fill(a, n) { for (var i = 0; i < n; i++) a[i] = i; return a; }
function pushAll(a, n) { for (var i = 0; i < n; i++) a.push(i); return a; }
function holey(a) { a[5] = "x"; return a; }
for (var k = 0; k < 5000; k++) { fill([], 8); pushAll([], 8); holey([1, 2]); }
var seen = "", seenN = 0;
Object.defineProperty(Array.prototype, "2", { set(v) { seen += v + ","; seenN++; }, get() { return "P"; }, configurable: true });
var a = fill([], 4), b = pushAll([], 4), c = holey([1]);
out.push(a.length + ":" + a.join(), b.length + ":" + b.join(), seen);
var lit = [7, 8, 9]; out.push(lit.hasOwnProperty(2), [1, 2, 3].map(x => x * 2).join(), Array.from([4, 5, 6]).join(), [0, 1].concat([2]).join());
var d = [1, 2]; d[2] = "own?"; out.push(d.hasOwnProperty(2), seenN);
for (var k = 0; k < 5000; k++) fill([], 3);
out.push(seenN);
delete Array.prototype[2];
var e = fill([], 4); out.push(e.join());
out.join(" | ");
