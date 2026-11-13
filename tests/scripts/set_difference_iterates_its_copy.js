// `difference` (when this set is the smaller) tests the copy's values,
// which `has` can't change, not this set's.
// expect-json: " | 1,2,3 | 1,3,7,9"

var out = [];
var s = new Set([1, 2]);
var other = { size: 2, has: function (v) { if (v === 1) s.delete(2); return true; }, keys: function () { return [][Symbol.iterator](); } };
out.push(Array.from(s.difference(other)).join(','));
s = new Set([1, 2, 3]);
other = { size: 100, has: function (v) { s.clear(); return false; }, keys: function () { return [][Symbol.iterator](); } };
out.push(Array.from(s.difference(other)).join(','));
s = new Set([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]);
other = { size: 100, has: function (v) { return v % 2 == 0 || v == 5; }, keys: function () { return [][Symbol.iterator](); } };
out.push(Array.from(s.difference(other)).join(','));
out.join(' | ');
