// Deletions past half a map's or a set's entries then insertions
// compact its entries (`collections.rs`): an iteration in progress
// goes on from the same entry in the new array. (As V8 and SpiderMonkey.)
// expect: 0=0,1=10,2=20,19=190,100=1,101=2,s1,s8,s9,s10

var m = new Map(), out = [];
for (var i = 0; i < 20; i++) m.set(i, i * 10);
for (var [k, v] of m) { out.push(k + "=" + v); if (k === 2) { for (var j = 3; j < 18; j++) m.delete(j); m.set(100, 1); m.set(101, 2); m.delete(18); } }
var s = new Set([1, 2, 3, 4, 5, 6, 7, 8]);
for (var x of s) { out.push("s" + x); if (x === 1) { [2, 3, 4, 5, 6, 7].forEach(y => s.delete(y)); s.add(9); s.add(10); } }
out.join()
