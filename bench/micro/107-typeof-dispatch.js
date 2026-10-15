// typeof tests on an array of mixed values: numbers, strings, objects, undefined, booleans.
// features: loop, string, object, array, call, typeof
// expect: 7100000
function weigh(v) {
  if (typeof v === "number") return v;
  if (typeof v === "string") return v.length;
  if (typeof v === "boolean") return v ? 1 : 0;
  if (typeof v === "undefined") return 3;
  if (typeof v === "object") return v === null ? 5 : 7;
  return 11;
}
function main() {
  var vals = [1, "ab", true, undefined, { a: 1 }, null, 2.5, "xyz", false, weigh];
  var s = 0;
  for (var r = 0; r < 200000; r++)
    for (var i = 0; i < vals.length; i++) s += weigh(vals[i]);
  return s;
}
main();
