// With `i` and `u` or `v`, case folds by Unicode's simple case folding
// (CaseFolding.txt's C and S mappings, `unicode_casefold::SIMPLE`):
// astral characters too (Deseret, Adlam: not folded at all before, as
// literals, classes, ranges, properties, complements, set operations and
// backreferences), and the BMP's S pairs (U+1FD3 and U+0390, U+FB05 and
// U+FB06), which upper and lower case don't relate. Legacy `i` still
// folds no astral character. What V8, SpiderMonkey and JavaScriptCore
// give; and, with Unicode 18's U+1DF95 folding to `ß` (a set of a BMP
// and an astral character), what the spec gives.
// expect: true true false true true true false true true true false true false false true false false true false true true x_y_ true true false true null null null

var D = "\u{10400}", d = "\u{10428}";   // Deseret capital/small long I
var r = [
  /^(\u{10400})\1$/iu.test(D + d), /^(.)\1$/iu.test(D + d), /^(.)\1$/u.test(D + d),
  /(?<=\1(.))x/iu.test(d + D + "x"), /^(?<a>\u{10428})\k<a>$/iu.test(d + D),
  /^[\u{10400}-\u{10402}]$/iu.test("\u{10429}"), /^[^\u{10400}]$/iu.test(d), /^[^\u{10400}]$/iu.test("\u{10429}"),
  /^\p{Lu}$/iu.test(d), /^\P{Lu}$/iu.test(D), /^[\p{Lu}--\u{10400}]$/iv.test(d), /^[\p{Lu}&&\u{10400}]$/iv.test(d),
  /^[[a-z]--[\u{10428}]]$/iv.test(D), /^[^\u{10400}]$/iv.test(d), /^[\q{\u{10400}x}]$/iv.test(d + "X"),
  /\u{10400}/i.test(d), /^[\u{10400}]$/i.test(d), /^\uD801$/iu.test("\uD801"), /^[\uD801]$/iu.test("𐐨"),
  /\u{1E900}/iu.test("\u{1E922}"), /^[\u{1E900}-\u{1E921}]+$/iu.test("\u{1E922}\u{1E943}"),
  "x\u{10400}y\u{10428}".replace(/\u{10428}/giu, "_"), /^\u{FB05}$/iu.test("\u{FB06}"), /^[\u{390}]$/iu.test("\u{1FD3}"),
  /^\u{1FD3}$/i.test("\u{390}"), /^(\u{FB05})\1$/iu.test("\u{FB05}\u{FB06}"),
  String(/(?<=\1(a))/.exec("a")), String(/(?<=\1(a))/iu.exec("a")), String(/(?<=\1(\u{10400}))/iu.exec("\u{10428}")),
];
r.join(" ");
