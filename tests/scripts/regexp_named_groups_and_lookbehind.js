// Named capture groups (`groups`, `\k<name>`, `$<name>`, a replacer's
// last argument) and lookbehinds, positive and negative.
// expect: groups 2020,01,31,,y,m,d | noGroups has | unmatched {} | backref "hi" | fwdNameRef true | replaceTpl 31/01/2020 | | replaceFn 1a2b | matchAll k1:v1,k2:v2 | dupName !SyntaxError | badName !SyntaxError | kWithoutNames true | behind 5,7 | negBehind 6 | behindVar 123,456 | behindAlt 1,3 | behindStart true,false | behindCapture ["b","a"] | behindReplace price: 10, cost: X | lookahead a1_2 | behindSticky true | behindNested b | hot 2000

var out = [];
function log() { out.push(Array.prototype.slice.call(arguments).map(String).join(" ")); }
function t(name, f) { try { log(name, f()); } catch (e) { log(name, "!" + e.name); } }
var m = "2020-01-31".match(/(?<y>\d{4})-(?<m>\d\d)-(?<d>\d\d)/);
t("groups", () => [m.groups.y, m.groups.m, m.groups.d, Object.getPrototypeOf(m.groups), Object.keys(m.groups).join()].join());
t("noGroups", () => "groups" in "a".match(/a/) ? "has" : "none");
t("unmatched", () => JSON.stringify("b".match(/(?<a>a)?b/).groups));
t("backref", () => /(?<q>['"]).*?\k<q>/.exec(`say "hi" 'x'`)[0]);
t("fwdNameRef", () => /\k<x>(?<x>a)/.test("a"));
t("replaceTpl", () => "2020-01-31".replace(/(?<y>\d+)-(?<m>\d+)-(?<d>\d+)/, "$<d>/$<m>/$<y> $<nope>|"));
t("replaceFn", () => "a1b2".replace(/(?<l>[a-z])(?<n>\d)/g, (...args) => { var g = args[args.length - 1]; return g.n + g.l; }));
t("matchAll", () => [..."k1=v1;k2=v2".matchAll(/(?<k>\w+)=(?<v>\w+)/g)].map(m => m.groups.k + ":" + m.groups.v).join());
t("dupName", () => new RegExp("(?<a>x)(?<a>y)"));
t("badName", () => new RegExp("(?<1a>x)"));
t("kWithoutNames", () => /\k/.test("k"));
t("behind", () => "$5 €6 $7".match(/(?<=\$)\d/g).join());
t("negBehind", () => "$5 €6 $7".match(/(?<!\$)\d/g).join());
t("behindVar", () => "abc123 xyz456".match(/(?<=[a-z]+)\d+/g).join());
t("behindAlt", () => "ab1 cd2 ef3".match(/(?<=ab|ef)\d/g).join());
t("behindStart", () => /(?<=^)x/.test("x") + "," + /(?<=a)x/.test("x"));
t("behindCapture", () => JSON.stringify("ab".match(/(?<=(a))b/)));
t("behindReplace", () => "price: 10, cost: 20".replace(/(?<=cost: )\d+/, "X"));
t("lookahead", () => "a1b2".replace(/[a-z](?=2)/, "_"));
t("behindSticky", () => { var r = /(?<=a)b/y; r.lastIndex = 1; return r.test("ab"); });
t("behindNested", () => "xaby".match(/(?<=(?<!q)a)b/)[0]);
t("hot", () => { var n = 0; for (var i = 0; i < 2000; i++) { if (/(?<=x)y/.test("axy" + i)) n++; } return n; });
out.join(" | ");
