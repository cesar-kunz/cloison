// In `u` mode a lone lead surrogate is a code point of its own, which a
// negated class not naming it matches (cloison-fuzz BUGS.md #5).
// expect: ["\ud800"]["a\ud800b"]null2

var J = JSON.stringify;
J(/[^x]/u.exec("\ud800")) + J(/[^x]+/u.exec("a\ud800b")) + J(/[^\ud800]/u.exec("\ud800")) + J(/[^x]/u.exec("😀")[0].length);
