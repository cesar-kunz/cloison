// A far array (`far.rs`) iterated by for-of and spread: every index up
// to its length, the holes `undefined`. (Past 2^31 an iteration takes
// 2^31 steps: `symbol::tests::array_states_step_past_int32_indices`
// steps there directly.)
// expect: 4000000,first/mid/last,4000000,mid,last,2147483649,3,below/above/

// A far array: elements high up, its length short enough to walk.
var a = []; a[0] = "first"; a[3000000] = "mid"; a[3999999] = "last";
var n = 0, seen = [];
for (var x of a) { n++; if (x !== undefined) seen.push(x); }
var s = [...a];
var b = []; b[2147483646] = "below"; b[2147483647] = "above"; b.length = 2147483649;
var r = b.slice(2147483646);
[n, seen.join("/"), s.length, s[3000000], s[3999999], b.length, r.length, r.join("/")].join()
