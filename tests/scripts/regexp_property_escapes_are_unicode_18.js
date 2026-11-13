// The properties are Unicode 18's, as the case folding is: `\P{L}` with
// `iu` doesn't match `ß` (U+1DF95, which folds to it, is a letter, not
// unassigned), nor do the other characters Unicode 18 folds new ones to;
// its new scripts are known.
// expect: false false false false false false false false true true true

[/\P{L}/iu.test("ß"), /\P{L}/iu.test("ẞ"), /\P{Cased}/iu.test("ß"), /\P{L}/iu.test("ɷ"),
  /\p{Cn}/iu.test("ß"), /\P{L}/iu.test("ꭋ"), /\P{Changes_When_Casefolded}/iu.test("ß"),
  /\P{Changes_When_Casefolded}/iu.test("ẞ"), /^\p{Ll}$/u.test("\u{1DF95}"),
  /^\p{sc=Seal}$/u.test("\u{3D000}"), /^\p{Script_Extensions=Jurchen}$/u.test("\u{18E00}")].join(" ");
