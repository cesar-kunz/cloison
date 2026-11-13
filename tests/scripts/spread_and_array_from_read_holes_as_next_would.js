// A source with holes is iterated, not copied (`append_dense` takes
// none with holes): each hole read through `Array.prototype`, whose
// getter here pushes onto the source, which the iteration then
// reaches (its length read at each step). As V8 and SpiderMonkey.
// expect: [1,"g",3,9] 4 [1,"g",3,9] 4 [0,null,2,4] 2

var out = "", a = [1, , 3];
Object.defineProperty(Array.prototype, 1, { get() { a.push(9); return "g"; }, configurable: true });
out += JSON.stringify([...a]) + " " + a.length;
a.length = 3;
out += " " + JSON.stringify(Array.from(a)) + " " + a.length;
var b = [, 2]; out += " " + JSON.stringify([0, ...b, 4]) + " " + b.length;
delete Array.prototype[1];
out
