// In `u` mode `.` and `\D`, `\W`, `\S` match a whole surrogate pair or a
// unit that is no pair's half, also when backtracking and in a lookbehind
// (cloison-fuzz BUGS.md #8).
// expect: ["Aé😀","é"]["","1"]null["ab😀!","😀"]

var J = JSON.stringify;
J(/(.){2,}./u.exec("Aé\u{1F600}\u2028")) + J(" \u{1F600}1".split(/../u)) + J(/a(?<=\B\W?a)/u.exec("c\u{1F600}a")) + J(/(\S){2,}./u.exec("ab\u{1F600}!"));
