// String methods taking regular expressions: replace (string and
// function replacements), split, match.
// for: regexp (3.7M replace), code-load, mandreel, typescript, pdfjs
// expect: The quick brown fox,a/b/c,x+y-z,a[b]c,Smith, John,a<1>b<2>c,a|b|c,1|22,
var s = "  The quick brown fox  ";
[s.replace(/^\s+|\s+$/g, ""), "a\\b\\c".replace(/\\/g, "/"), "x-y-z".replace("-", "+"),
 "abc".replace(/b/, "[$&]"), "John Smith".replace(/(\w+)\s(\w+)/, "$2, $1"),
 "a1b22c".replace(/\d+/g, function (d) { return "<" + d.length + ">"; }),
 "a, b,c".split(/\s*,\s*/).join("|"), ("x1y22z".match(/\d+/g) || []).join("|"), "abc".match(/z/)].join(",");
