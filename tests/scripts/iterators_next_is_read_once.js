// An iterator's `next` is read once, when the iteration starts (as
// GetIterator records it), by `for … of`, spread, destructuring,
// `yield*` and the builtins that iterate.
// expect: 0,1,2,1,3,1,0,1,1,1,012,1,3,3,3,3

var reads = 0;
function iterable() {
  var i = 0;
  return { [Symbol.iterator]() {
    return { get next() { reads++; return function () { return { done: i >= 3, value: i++ }; }; } };
  } };
}
var r = [];
for (var x of iterable()) r.push(x);
r.push(reads); reads = 0;
r.push([...iterable()].length, reads); reads = 0;
var [a, b, ...c] = iterable();
r.push(a, b, c.length, reads); reads = 0;
function* g() { yield* iterable(); }
r.push([...g()].join(""), reads); reads = 0;
r.push(new Set(iterable()).size, Array.from(iterable()).length, new Uint8Array(iterable()).length, reads);
r.join();
