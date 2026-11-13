// With `i` and `u` or `v`, case folds by Unicode's simple case folding
// (CaseFolding.txt's C and S mappings, `unicode_casefold::SIMPLE`):
// astral characters too (Deseret, Adlam: not folded at all before, as
// literals, classes, ranges, properties, complements, set operations and
// backreferences), and the BMP's S pairs (U+1FD3 and U+0390, U+FB05 and
// U+FB06), which upper and lower case don't relate. Legacy `i` still
// folds no astral character. What V8, SpiderMonkey and JavaScriptCore
// give; and, with Unicode 18's U+1DF95 folding to `ß` (a set of a BMP
// and an astral character), what the spec gives.
// expect: true true true true true false false true true true true false false true

var L = "\u{1DF95}";
[/^ß$/iu.test(L), /^\u{1E9E}$/iu.test(L), /^\u{1DF95}$/iu.test("ß"), /^\u{1DF95}$/iu.test("\u{1E9E}"),
  /^[ß]$/iu.test(L), /^[^ß]$/iu.test(L), /^[^\u{1DF95}]$/iu.test("ß"), /^[\u{1DF95}]$/iv.test("\u{1E9E}"),
  /^(ß)\1$/iu.test("ß" + L), /^(\u{1DF95})\1$/iu.test(L + "\u{1E9E}"), /(?<=(ß)\1)x/iu.test("ß" + L + "x"),
  /^ß$/i.test(L), /^ß$/u.test(L), /^[a-zß]+$/iu.test("ab" + L)].join(" ");
