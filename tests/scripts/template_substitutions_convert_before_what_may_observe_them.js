// A substitution followed only by literals and locals no call can set
// converts with the rest, at the end; one followed by a local a closure
// captures, a parameter a mapped `arguments` aliases, or a `let` before
// its declaration, before them (cloison-fuzz BUGS.md #17).
// expect: P1b2  Q2 Q9 Q8 Q15 p,p,ReferenceError

var log = [];
var p = { toString() { log.push("p"); return "P"; } };
function locals() { var a = 1, b = "b"; return `${p}${a}${b}${2}`; }
function tdz() { try { return `${p}${x}`; } catch (e) { log.push(e.constructor.name); } let x = 1; }
function captured() { var a = 1; var q = { toString() { a = 2; return "Q"; } }; return `${q}${a}`; }
function mapped(a) { var q = { toString: () => { arguments[0] = 9; return "Q"; } }; return `${q}${a}`; }
function aliased(a) { var args = arguments; var q = { toString() { args[0] = 8; return "Q"; } }; return `${q}${a}`; }
function param(a, b) { var q = { toString() { b = 5; return "Q"; } }; return `${q}${a}${b}`; }
[locals(), tdz(), captured(), mapped(1), aliased(1), param(1, 2), log.join()].join(" ");
