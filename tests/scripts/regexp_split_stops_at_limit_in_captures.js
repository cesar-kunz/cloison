// `split` with a regular expression stops as soon as the result reaches
// the limit, also when a capture reaches it (cloison-fuzz BUGS.md #4).
// expect: ["a","1"]["a"]["a","1","b"]

var J = JSON.stringify;
J("a1b2c".split(/(\d)/, 2)) + J("a1b2c".split(/(\d)/, 1)) + J("a1b2c".split(/(\d)/, 3));
