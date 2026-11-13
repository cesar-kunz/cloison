// `f.apply(that, [])`: the literal has no elements allocated (`elems`
// null), which the list of the call must not make a slice of (the debug
// build aborted), from interpreted and compiled callers.
// expect: 4000

function count() { return arguments.length; }
var t = 0;
for (var i = 0; i < 4000; i++) t += count.apply(null, []) + count.apply(null, [i]);
t;
