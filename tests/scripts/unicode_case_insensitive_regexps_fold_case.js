// With `i` and `u`, case compares by Unicode simple case folding: the
// Kelvin sign is `k`, `ẞ` is `ß`, `ſ` is `s` (and a word character),
// `ς` is `σ`; `ı` is only itself. Without `u` none of them.
// expect: true true false true false true false false true true false true true x__

var r = [/K/iu.test("k"), /k/iu.test("K"), /k/i.test("K"), /ß/iu.test("ẞ"), /ß/i.test("ẞ"),
  /\w/iu.test("ſ"), /\w/i.test("ſ"), /\W/iu.test("ſ"), /[a-z]/iu.test("K"), /s/iu.test("ſ"),
  /i/iu.test("ı"), /σ/iu.test("ς"), /\bſ/iu.test("ſ"), "xKk".replace(/k/giu, "_")];
r.join(" ");
