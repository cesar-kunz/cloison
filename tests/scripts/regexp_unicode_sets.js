// The RegExp `v` flag: classes as sets, with nested classes,
// intersection, subtraction, strings, and properties of strings.
// expect: true,false,ae | abc,true,false | true,true | true,false,v,false | SyntaxError | SyntaxError | SyntaxError | SyntaxError | SyntaxError | SyntaxError

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => [/[\p{L}--[a-z]]/v.test("A"), /[\p{L}--[a-z]]/v.test("a"), /[[a-z]&&[aeiou]]+/v.exec("xyzaeb")[0]].join());
t(() => [/[\q{abc|d}x]/v.exec("zabc")[0], /^[\q{}]$/v.test(""), /[^a-z]/vi.test("Q")].join());
t(() => [/^\p{RGI_Emoji}$/v.test("\u{1F468}‍\u{1F469}‍\u{1F467}"), /^\p{Emoji_Keycap_Sequence}+$/v.test("1️⃣2️⃣")].join());
t(() => [/x/v.unicodeSets, /x/v.unicode, /x/v.flags, /x/u.unicodeSets].join());
t(() => new RegExp("[^\\p{RGI_Emoji}]", "v"));
t(() => new RegExp("\\P{RGI_Emoji}", "v"));
t(() => new RegExp("[a&&&b]", "v"));
t(() => new RegExp("[a-z--b]", "v"));
t(() => new RegExp("[(]", "v"));
t(() => new RegExp("x", "uv"));
r.join(" | ");
