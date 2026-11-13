// A species that returns the source array itself: `slice` copies the
// range onto it (not overlapping as one copy), `splice`'s `length` set on
// the removed array truncates the source before the shift; the other
// species users write it element by element.
// expect: 3,4,5,6 | 2,3,4,5 | b,c | 6:3,4,9,,, | 6:2,7,8,9,, | 1,2,3,4,5 | 2,4,6 | 1,3,3,4 | 1,2,3,3 | 1,1,1,1,1,1

function self(a) { a.constructor = { [Symbol.species]: function () { return a; } }; return a; }
var r = [];
var a = self([1, 2, 3, 4, 5, 6, 7, 8]); r.push(a.slice(2, 6).join());
var a = self([1.5, 2, 3, 4, 5]); r.push(a.slice(1).join());
var a = self(["a", "b", "c", "d"]); r.push(a.slice(1, 3).join());
var a = self([1, 2, 3, 4, 5, 6, 7, 8]); a.splice(2, 3, 9); r.push(a.length + ":" + a.join());
var a = self([1, 2, 3, 4]); a.splice(1, 1, 7, 8, 9); r.push(a.length + ":" + a.join());
var a = self([1, 2, 3]); r.push(a.concat([4], 5).join());
var a = self([1, 2, 3]); r.push(a.map(x => x * 2).join());
var a = self([1, 2, 3, 4]); r.push(a.filter(x => x % 2).join());
var a = self([1, [2, 3], 4]); r.push(a.flat().join());
var a = self([1, 2, 3]); r.push(a.flatMap(x => [x, x]).join());
r.join(" | ");
