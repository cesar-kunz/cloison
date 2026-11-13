// RegExp modifiers: `(?ims-ims:…)` turn `i`, `m` and `s` on or off
// inside the group only.
// expect: true,false,true,false | AbCD | true,false,false | true,false | true,false,true | true,true | SyntaxError | SyntaxError | SyntaxError | SyntaxError

var r = [];
function t(f) { try { r.push(String(f())); } catch (e) { r.push(e.name); } }
t(() => [/a(?i:b)c/.test("aBc"), /a(?i:b)c/.test("aBC"), /(?i:a(?-i:b))/.test("Ab"), /(?i:a(?-i:b))/.test("AB")].join());
t(() => /(?i:[a-c]+)D/.exec("xAbCD")[0]);
t(() => [/(?m:^b)/.test("a\nb"), /^b/.test("a\nb"), /(?-m:^b)/m.test("a\nb")].join());
t(() => [/(?s:a.b)/.test("a\nb"), /a(?-s:.)b/s.test("a\nb")].join());
t(() => [/(a)(?i:\1)/.test("aA"), /(a)\1/.test("aA"), /(?i:(a)\1)/.test("aA")].join());
t(() => [/(?i:\u212A)/u.test("k"), /(?i:\b)ſ/u.test("ſ")].join());
t(() => new RegExp("(?ii:a)"));
t(() => new RegExp("(?i-i:a)"));
t(() => new RegExp("(?-:a)"));
t(() => new RegExp("(?x:a)"));
r.join(" | ");
