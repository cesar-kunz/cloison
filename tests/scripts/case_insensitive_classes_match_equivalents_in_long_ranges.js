// With `i`, a class matches a unit whose case-equivalent is in it, however
// long its ranges (#43): one of 512 units or more matched only the unit
// itself or its canonical form, not a member folding to that form (`ſ`
// in `[\u0100-\u0300]` for `s`). Legacy `i` still never maps `ſ` to `s`.
// expect: true false true true true true true false true x_y sS,s true true true true true true

var r = [/[\u0100-\u0300]/iu.test("s"), /[^\u0100-\u0300]/iu.test("s"), /[\u2126-\u2400]/iu.test("\u03c9"),
  /[\u212a-\u2400]/iu.test("k"), /[\u1e9e-\u2100]/iu.test("\u00df"), /[\u1c80-\u1e80]/i.test("\u0432"),
  /[\u0100-\u0300]/iv.test("s"), /[\u017f-\u037f]/i.test("s"), /^[\u0100-\u0300]+$/iu.test("sS\u017f"),
  "xSy".replace(/[\u0100-\u0300]/giu, "_"), /([\u0100-\u0300])\1/iu.exec("sS"), /(?<=[\u0100-\u0300])x/iu.test("sx")];
for (var n of [510, 511, 512, 513, 2000])
  r.push(new RegExp("[\\u017f-\\u" + (0x17f + n).toString(16).padStart(4, "0") + "]", "iu").test("s"));
r.join(" ");
