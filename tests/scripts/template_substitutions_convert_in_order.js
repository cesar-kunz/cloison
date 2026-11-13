// A template literal's substitution is converted (ToString, hint
// "string") before the next one is evaluated, which may observe it or
// throw (cloison-fuzz BUGS.md #17).
// expect: ss|1|s string,string,mid,string,string,threw,TypeError

var log = [];
var p = { [Symbol.toPrimitive](h) { log.push(h); return h === "string" ? "s" : 5; } };
var t = `${p}${p}|${(log.push("mid"), 1)}|${p}`;
try { `${p}${(() => { throw 0; })()}`; } catch (x) { log.push("threw"); }
try { `${Symbol()}${(log.push("after"), 0)}`; } catch (x) { log.push(x.constructor.name); }
[t, log.join(",")].join(" ");
