// A pattern starting with an empty class has no first unit: it matches
// nothing, where the scan for its first units indexed an empty set and
// panicked (cloison-fuzz BUGS.md #1).
// expect: nullnull["x"]["ab"]

var J = JSON.stringify;
J(/[]/.exec("abcdefg")) + J(/[]a/.exec("aaa")) + J(/[^]/.exec("x")) + J("ab".split(/[]/));
