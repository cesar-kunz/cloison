// A raw lone surrogate in a pattern string is a pattern character, not
// the text `\uD83D`: it doesn't join an escaped surrogate next to it,
// a backslash before it is its identity escape (a SyntaxError in `u` and
// `v` mode), and `source` keeps it raw, apart from the escape `\uD83D`
// (#122). With and without `u`, in classes, and through `compile`.
// expect: false,true,false,true,false,false,true,false,true,SyntaxError,SyntaxError,false,true,true,false,true,true,true,"\ud83d","/a\ude00\\//u","\\uD83D",false,false,true,false,"\ud83d\\uDE00",1

var H = "\ud83d", L = "\ude00", P = "\u{1F600}", J = JSON.stringify;
function R(p, f) { var n; try { return new RegExp(p, f); } catch (e) { n = e.name; } return { test: () => n, source: n }; }
var r = [R(H + "\\ude00", "u").test(P), R("^\\" + H + "$").test(H), R("^\\" + H + "$").test("\\uD83D"),
  R(H + "\\uDE00").test(P), R(H + "\\uDE00", "v").test(P), R("\\uD83D" + L, "u").test(P), R("^" + H + L + "$", "u").test(P),
  R(H, "u").test(P), R(L, "u").test(L), R("\\" + H, "u").test(H), R("\\" + H, "v").test(H),
  R("^[" + H + "\\uDE00]$", "u").test(P), R("^[" + H + "\\uDE00]$").test(L), R("^[\\" + H + "]$").test(H), R("^[\\" + H + "]$").test("\\"),
  R("^[" + H + "-" + L + "]$", "u").test("\ud8ff"), R("^[\\q{" + H + "}]$", "v").test(H), R(H, "iu").test(H),
  J(R(H).source), J(String(R("a" + L + "/", "u"))), J(R("\\uD83D").source), R(H).source === R("\\uD83D").source,
  R(H).test("\\uD83D"), R(new RegExp("\\" + H), "g").test(H)];
var re = /a/; re.compile(H + "\\uDE00", "u");
r.push(re.test(P), J(re.source), ("x\\" + H).search("\\\\" + H));
r.join();
