// In `u` mode the input is code points: an empty match advances past a
// surrogate pair, and a lone surrogate (a character, a class member)
// matches alone, not half of a pair; an escaped pair is its code point.
// expect: 4 2 2 1 null null null 0:1 0:1 1:1 0:2 true true true false

var e = "\u{1F600}", r = [];
function s(m) { return m === null ? "null" : m.index + ":" + m[0].length; }
r.push(e.replace(/(?:)/gu, "-").length, e.match(/(?:)/gu).length, [...e.matchAll(/(?:)/gu)].length, e.split(/(?:)/u).length);
r.push(s(/\ud83d/u.exec(e)), s(/\ude00/u.exec(e)), s(/[\ud83d]/u.exec(e)), s(/\ud83d/.exec(e)));
r.push(s(/\ud83d/u.exec("\ud83dx")), s(/[\ud800-\udfff]/u.exec("a\ud800b")), s(/😀/u.exec(e)));
r.push(/^.$/u.test(e), /^.$/su.test(e), /^[^a]$/u.test(e), /^.\ude00$/u.test(e));
r.join(" ");
