// Annex B lets a lookahead be quantified outside `u` mode, never a
// lookbehind (#148).
// expect: SyntaxError SyntaxError SyntaxError ok ok SyntaxError true

function t(p, f) { try { new RegExp(p, f); return "ok"; } catch (e) { return e.name; } }
[t("(?<=a){2}"), t("(?<!a)*"), t("(?<=a)?", "u"), t("(?=a){2}"), t("(?!a)*"), t("(?=a)+", "u"), /(?=a)*a/.test("a")].join(" ");
