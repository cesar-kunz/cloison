// In `u` mode a search, and `split`'s sticky one, never starts between a
// surrogate pair's halves: it advances by code points
// (AdvanceStringIndex; cloison-fuzz BUGS.md #6).
// expect: ["a"]132

var J = JSON.stringify;
J(/[^\u{1F600}]/u.exec("\u{1F600}a")) + J("a\u{1F600}".split(/\B/uy).length) + /\B/u.exec("A\u{1F600}").index + J("\u{1F600}\u{1F600}".split(/(?:)/u).length);
