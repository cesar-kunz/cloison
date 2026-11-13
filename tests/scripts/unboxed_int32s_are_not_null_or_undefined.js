// In the interpreter's int32 state the accumulator is unboxed, and the
// int32s 2 and 10 have `null`'s and `undefined`'s bits: neither is
// either.
// expect: 0 false false

var r = 0;
for (var i = 0; i < 1000; i++) {
  var x = (i % 12) + 0;
  if (x === undefined) r += 1000;
  if (x === null) r += 100;
  if (x == null) r += 10;
  if ((x ?? -1) < 0) r += 1;
}
[r, (10 + 0) === undefined, (2 + 0) === null].join(" ");
