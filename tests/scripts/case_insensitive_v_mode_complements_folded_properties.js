// With `i` and `v`, `\P{…}` is the complement of the property folded,
// among the code points folding leaves as they are, as a negated class
// is (#107): for a cased property, neither case of a letter in it.
// With `u` alone, the complement of the property among all code points.
// expect: false true false true false false true true false false true

[/\P{Lu}/iv.test("a"), /[^\P{Lu}]/iv.test("a"), /\P{Ll}/iv.test("A"), /\P{Lu}/iu.test("a"),
  /[\P{Lu}]/iv.test("a"), /[\P{Lu}--x]/iv.test("a"), /\P{Lu}/iv.test("1"), /\P{Lu}/v.test("a"),
  /\P{Lowercase}/iv.test("K"), /\P{Cased}/iv.test("\u017f"), /[\P{Ll}&&\P{Lu}]/iv.test("1")].join(" ");
