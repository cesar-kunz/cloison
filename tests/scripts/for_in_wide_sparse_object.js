// A wide sparse object's index keys (stored as names: see
// `Vm::sparse_key`): each once, in order, a non-enumerable one skipped.
// (Each index once looked up its attributes in a list: O(n²).)
// expect: 3000 7 x -1 2999007

var n = 3000, o = {}, c = 0, first, last, k;
for (var i = n - 1; i >= 0; i--) o[i * 1000 + 7] = i;
o.x = 1;
Object.defineProperty(o, 1007, { enumerable: false });
for (k in o) { if (first === undefined) first = k; last = k; c++; }
[c, first, last, Object.keys(o).indexOf("1007"), Object.keys(o)[2998]].join(" ");
