// `typeof` in compiled code: of every kind of value, known types and
// unknown ones, in an inlined function, and of globals: one never
// defined, one made and deleted while the loop runs.
// for: pdfjs, typescript, earley-boyer, mandreel, raytrace
// expect: boolean:2500,function:5000,number:5000,object:12500,string:2500,undefined:2500|30000|30000|10000|number|function
function kind(v) { return typeof v; }
var vals = [1, 1.5, "s", true, undefined, null, {}, [], function () {}, Math.max, /r/, new Date(0)];
var counts = {}, n = 0, missing = 0, seen = 0;
for (var i = 0; i < 30000; i++) {
  var v = vals[i % vals.length];
  var k = kind(v);
  counts[k] = (counts[k] || 0) + 1;
  if (typeof i === "number") n++;
  if (typeof neverDefined === "undefined") missing++;
  if (i === 10000) this.later = 1;
  if (i === 20000) delete this.later;
  if (typeof later !== "undefined") seen++;
}
var names = [];
for (var key in counts) names.push(key + ":" + counts[key]);
[names.sort().join(","), n, missing, seen, typeof "x".length, typeof kind].join("|");
