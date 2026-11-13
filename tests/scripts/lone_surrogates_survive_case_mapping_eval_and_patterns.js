// A lone surrogate survives `toLowerCase`/`toUpperCase`, `eval`'d and
// `Function`'s source, and a pattern given as a string: not U+FFFD.
// expect: 55296 56320 ας 55296 56320 55357 true false 1 55296 55296

var r = ["\ud800a".toLowerCase().charCodeAt(0), "a\udc00B".toLowerCase().charCodeAt(1), "ΑΣ".toLowerCase(),
  eval("'" + "\ud800" + "'").charCodeAt(0), Function("return '" + "\udc00" + "'")().charCodeAt(0),
  (0, eval)("'\ud83d'").charCodeAt(0), new RegExp("\ud800").test("\ud800"), new RegExp("\ud800").test("\ud801"),
  "a\ud800b".search("\ud800"), eval("`" + "\ud800" + "`").charCodeAt(0)];
(function () { r.push(eval("'" + "\ud800" + "'").charCodeAt(0)); })();
r.join(" ");
