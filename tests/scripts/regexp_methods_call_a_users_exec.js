// RegExp's methods and String's through them go through a user's `exec`
// (or a pattern's own `Symbol.replace`…), as the spec's generic
// algorithms do; `flags` asks each flag's getter.
// expect: replaceCustomExec=aXcX exec@0,exec@2,exec@4 matchCustomExec=b,b 3 testCustom=true searchCustom=7 splitCustom=a|b|c fakeObj=abc flagsGeneric=gy flagsReal=gimsuy replaceFnGroups=[1]x[2] namedTemplate=y<2020> execBadResult=!TypeError symReplaceOverride=custom:axb! species=a|b matchAllCustom=1,2 fastStill=a#b#|x/y|1

var out = [];
function t(n, f) { try { out.push(n + "=" + f()); } catch (e) { out.push(n + "=!" + e.name); } }
var calls = [];
var re = /b/g;
re.exec = function (s) { calls.push("exec@" + this.lastIndex); return RegExp.prototype.exec.call(this, s); };
t("replaceCustomExec", () => "abcb".replace(re, "X") + " " + calls.join());
calls = [];
t("matchCustomExec", () => "abcb".match(re).join() + " " + calls.length);
t("testCustom", () => { var r = /x/; r.exec = () => ({ 0: "fake", index: 0 }); return r.test("nope"); });
t("searchCustom", () => { var r = /x/; r.exec = () => ({ index: 7 }); return "abc".search(r); });
t("splitCustom", () => { var r = /,/; return "a,b,c".split(r).join("|"); });
t("fakeObj", () => RegExp.prototype[Symbol.replace].call({ exec() { return null; }, flags: "" }, "abc", "X"));
t("flagsGeneric", () => Object.getOwnPropertyDescriptor(RegExp.prototype, "flags").get.call({ global: 1, sticky: true, multiline: 0 }));
t("flagsReal", () => /a/gimsuy.flags);
t("replaceFnGroups", () => { var r = /(?<a>\d)/g; r.exec = function (s) { return RegExp.prototype.exec.call(this, s); }; return "1x2".replace(r, (m, p1, off, str, groups) => "[" + groups.a + "]"); });
t("namedTemplate", () => { var r = /(?<y>\d+)/; r.exec = function (s) { return RegExp.prototype.exec.call(this, s); }; return "y2020".replace(r, "<$<y>>"); });
t("execBadResult", () => { var r = /x/; r.exec = () => 5; return r.test("x"); });
t("symReplaceOverride", () => { var r = /x/; r[Symbol.replace] = (s, v) => "custom:" + s + v; return "axb".replace(r, "!"); });
t("species", () => { class R extends RegExp { static get [Symbol.species]() { return RegExp; } } var r = new R("-"); return "a-b".split(r).join("|"); });
t("matchAllCustom", () => { var r = /\d/g; r.exec = function (s) { return RegExp.prototype.exec.call(this, s); }; return [..."a1b2".matchAll(r)].map(m => m[0]).join(); });
t("fastStill", () => "a1b22".replace(/\d+/g, "#") + "|" + "x,y".split(/,/).join("/") + "|" + "abc".match(/b/).index);
out.join(" ");
