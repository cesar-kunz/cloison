// A `\x`, `\u` or `\u{…}` escape takes hex digits only, not a sign: in
// Annex B `\x+1` is `x` then `+1`, and in `u` mode a SyntaxError (#123).
// expect: true true true true SyntaxError SyntaxError SyntaxError true

function t(p) { try { new RegExp(p, "u"); return "ok"; } catch (e) { return e.name; } }
[/^\x+1$/.test("xx1"), new RegExp("^\\x+1$").test("xx1"), /^\u+0041$/.test("uu0041"),
 /^[\x+1]+$/.test("x+1"), t("\\x+1"), t("\\u{+41}"), t("[\\u{+41}]"), /\x41A/.test("AA")].join(" ");
