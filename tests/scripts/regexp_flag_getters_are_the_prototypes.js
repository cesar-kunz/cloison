// A RegExp's flags and source are `RegExp.prototype`'s accessors, not
// its own properties (#191): redefined there, `re.global`, the `flags`
// getter and the methods reading `flags` see them (once the fast paths
// have run, too).
// expect: axax3 lastIndex a\/b (?:) c\/d  1 false axab 2 ["b"] 4 TypeError TypeError 6 m /a/im 3

var r = [];
function t(f) { try { return String(f()); } catch (e) { return e.name; } }
var s; for (var i = 0; i < 2000; i++) s = "abab".replace(/b/g, "x") + "abab".split(/b/).length;
r.push(s, Object.getOwnPropertyNames(/a/g).join(), /a\/b/.source, new RegExp("").source, /a/.compile("c/d").source);
var cnt = 0;
Object.defineProperty(RegExp.prototype, "global", { get() { cnt++; return false; } });
r.push(/z/g.flags, cnt, /z/g.global);
var re = /b/g;
r.push("abab".replace(re, "x"), re.lastIndex, JSON.stringify("abab".match(/b/g)), cnt);
r.push(t(() => "abab".replaceAll(/b/g, "x")), t(() => [..."abab".matchAll(/b/g)].length), cnt);
Object.defineProperty(RegExp.prototype, "multiline", { get() { return true; } });
r.push(/a/.flags, String(/a/i), "abab".split(/b/).length);
r.join(" ");
