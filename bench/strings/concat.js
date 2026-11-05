// Concatenation scaling (#14): strings built by `+=` and by prepending, of
// constant, numeric and mixed operands, at growing lengths. Each build is
// timed apart from the result's first materialization (a flat copy of a
// rope) and from consuming it after (reads, a search), medians of REPS
// runs. Linear building takes 8x longer for 8x the pieces; quadratic
// copying, 64x: a ratio past 24 is flagged.
//
// Prints one line per configuration and length, tab-separated (ms), then
// a verdict per configuration. Engines: cloison, d8, sm, jsc (`print`).

var REPS = 5;
var SIZES = [100000, 200000, 400000, 800000];
var now = typeof performance !== "undefined" && performance.now ? function () { return performance.now(); } : Date.now;
var parts = ["x", "😀", "é", 7, true, 1.5, "abc"];
var configs = {
  "append-const": function (n) { var s = ""; for (var i = 0; i < n; i++) s += "ab"; return s; },
  "prepend-const": function (n) { var s = ""; for (var i = 0; i < n; i++) s = "ab" + s; return s; },
  "append-int": function (n) { var s = ""; for (var i = 0; i < n; i++) s += i; return s; },
  "append-double": function (n) { var s = ""; for (var i = 0; i < n; i++) s += i * 0.5; return s; },
  "append-mixed": function (n) { var s = ""; for (var i = 0; i < n; i++) s += parts[i % parts.length]; return s; },
  "both-ends": function (n) { var s = ""; for (var i = 0; i < n; i++) s = (i & 1 ? "a" + s : s + "b"); return s; },
};

function median(xs) { xs.sort(function (a, b) { return a - b; }); return xs[xs.length >> 1]; }

var lines = ["config\tpieces\tlength\tbuild\tfirst\tconsume"], verdicts = [], check = 0;
for (var name in configs) {
  var f = configs[name], builds = [];
  for (var k = 0; k < SIZES.length; k++) {
    var n = SIZES[k], b = [], m = [], c = [], len = 0;
    for (var r = 0; r < REPS; r++) {
      var t0 = now();
      var s = f(n);
      var t1 = now();
      check = (check + s.charCodeAt(s.length >> 1)) | 0;
      var t2 = now();
      for (var i = 0; i < 2000; i++) check = (check + s.charCodeAt((i * 7919) % s.length)) | 0;
      check = (check + s.indexOf("ab", s.length >> 2) + s.lastIndexOf("x")) | 0;
      var t3 = now();
      b.push(t1 - t0); m.push(t2 - t1); c.push(t3 - t2); len = s.length;
    }
    builds.push(median(b));
    lines.push([name, n, len, median(b).toFixed(2), median(m).toFixed(2), median(c).toFixed(2)].join("\t"));
  }
  var ratio = builds[builds.length - 1] / Math.max(builds[0], 0.05);
  verdicts.push(name + ": " + ratio.toFixed(1) + "x for " + SIZES[SIZES.length - 1] / SIZES[0] + "x the pieces" + (ratio > 24 ? " QUADRATIC" : ""));
}
print(lines.join("\n"));
print(verdicts.join("\n"));
print("checksum " + check);
