// `union` and `symmetricDifference` get the other's keys iterator (its
// `keys()` and `next`) before copying this set, which they may change.
// expect: 1,2 | 1,2 | 1,4,3 | 1,4,6

var out = [];
var s = new Set([1]);
var other = { size: 0, has: function () { return false; }, keys: function () { s.add(2); return [][Symbol.iterator](); } };
out.push(Array.from(s.union(other)).join(','));
s = new Set([1]);
other = { size: 0, has: function () { return false; }, keys: function () { s.add(2); return [][Symbol.iterator](); } };
out.push(Array.from(s.symmetricDifference(other)).join(','));
s = new Set([1]);
other = { size: 0, has: function () { return false }, keys: function () { var it = [3][Symbol.iterator](); return { get next() { s.add(4); return function () { return it.next(); }; } }; } };
out.push(Array.from(s.union(other)).join(','));
s = new Set([1, 5]);
other = { size: 0, has: function () { return false }, keys: function () { var it = [5, 6][Symbol.iterator](); return { get next() { s.add(4); return function () { var r = it.next(); s.add(9); return r; }; } }; } };
out.push(Array.from(s.symmetricDifference(other)).join(','));
out.join(' | ');
