// A template literal writes an integer substitution's digits itself, no
// string made for it: as ToString's for zero, negatives and int32's
// bounds, with doubles, -0, objects (their `toString` called in order)
// and strings beside them unchanged, and a symbol still a TypeError.
// expect: 0|-1|7_-7|2147483647,-2147483648|4294967296|0|1.5e+21|a12b-34c|<1,2>|x1y2|TypeError|ab|1234567890-1234567890
var out = [];
function one(v) { return `${v}`; }
function two(a, b) { return `${a}_${b}`; }
var zero = 0, big = 2147483647, small = -2147483648;
out.push(one(zero), one(-1), two(7, -7), `${big},${small}`, one(big * 2 + 2), one(-zero), one(1.5e21));
out.push(`a${12}b${-34}c`);
out.push(`<${[1, 2]}>`);
var log = "";
var x = { toString() { log += "x"; return "x"; } }, y = { toString() { log += "y"; return "y"; } };
out.push(`${x}${1}${y}${2}`);
try { one(Symbol("s")); } catch (e) { out.push(e.constructor.name); }
out.push(log == "xy" ? "ab" : log);
var n = 1234567890;
for (var i = 0, s = ""; i < 2000; i++) s = two(n, -n);
out.push(s.replace("_", ""));
out.join("|");
