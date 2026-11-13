// `normalize` in each form: compatibility mappings (pdf.js NFKC-maps a
// ligature to its letters), singletons, Hangul, marks reordered and
// composed, composition exclusions, lone surrogates left alone. The
// expected values are V8's.
// expect: 66.69 fb01 c5 41.30a 1112.1161.11ab d55c 1ead 61.323.302 78.d800.79 31.31.2044.32 f71.f72 1d157.1d165

var hex = s => [...s].map(c => c.codePointAt(0).toString(16)).join(".");
var r = [
  hex("ﬁ".normalize("NFKC")), hex("ﬁ".normalize()),
  hex("Å".normalize()), hex("Å".normalize("NFD")),
  hex("한".normalize("NFD")), hex("한".normalize()),
  hex("ậ".normalize()), hex("ậ".normalize("NFD")),
  hex("x\uD800y".normalize("NFKD")), hex("①½".normalize("NFKC")),
  hex("ཱི".normalize()), hex("\u{1D15E}".normalize()),
];
r.join(" ");
