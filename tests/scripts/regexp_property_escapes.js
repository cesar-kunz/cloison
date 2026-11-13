// Unicode property escapes (`u` mode): binary properties, general
// categories and scripts by any of their names, negated, in classes,
// astral code points (`\u{…}` and surrogate pair escapes in classes
// too); an unknown name is a SyntaxError, early in a literal.
// expect: true true true false 12!? true true true true SyntaxError SyntaxError true false true true aXbXc true false true SyntaxError true false SyntaxError

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => /\p{L}/u.test("é")); t(() => /\p{Script=Greek}/u.test("α")); t(() => /^\p{Lu}+$/u.test("ABÇ"));
t(() => /\P{L}/u.test("a")); t(() => /[\p{N}\p{P}]+/u.exec("ab12!?c")[0]);
t(() => /\p{Emoji_Presentation}/u.test("😀")); t(() => /^[\u{1F600}-\u{1F64F}]$/u.test("😃"));
t(() => /^\p{Script=Han}$/u.test("𠀀")); t(() => /^\p{sc=Hani}$/u.test("中"));
t(() => new RegExp("\\p{Foo}", "u")); t(() => new RegExp("\\p{ascii}", "u")); t(() => /\p{L}/.test("p{L}"));
t(() => /[^\p{L}]/u.test("a")); t(() => /^[^\p{L}]$/u.test("😀")); t(() => /\p{Lu}/iu.test("a"));
t(() => "a😀b😃c".replace(/\p{Emoji_Presentation}/gu, "X")); t(() => /^[😀]$/u.test("😀"));
t(() => /^\P{Any}$/u.test("a")); t(() => /^\p{General_Category=Decimal_Number}+$/u.test("٣4"));
t(() => new RegExp("[\\p{L}-z]", "u")); t(() => /^[^a]$/u.test("😀")); t(() => /^[^a]$/iu.test("A"));
t(() => eval("/\\p{Nope}/u; 1"));
r.join(" ");
