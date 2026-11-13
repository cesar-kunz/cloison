// Array destructuring closes its iterator where a default, a nested
// pattern or a target throws, and evaluates a target's object and key
// before its step (a rest's before draining the iterator).
// expect: tnR setnR tR tnR 12nnR 21

var r = [];
function mk(vals, forever) { var log = []; var i = 0; return [log, { [Symbol.iterator]() { return {
  next() { log.push("n"); return forever || i < vals.length ? { done: false, value: vals[i++] } : { done: true }; },
  return() { log.push("R"); return {}; } }; } }]; }
function thrower() { throw new Error("t"); }
function run(f) { try { f(); return "ok"; } catch (e) { return e.message; } }
var [l, it] = mk([undefined]); r.push(run(() => { var [a = thrower()] = it; }) + l.join(""));
[l, it] = mk([1]); var o = { set x(v) { throw new Error("set"); } }; r.push(run(() => { [o.x] = it; }) + l.join(""));
[l, it] = mk([1]); r.push(run(() => { [{}[thrower()]] = it; }) + l.join(""));
[l, it] = mk([], true); r.push(run(() => { var a; [a, ...[thrower()][0]] = it; }) + l.join(""));
[l, it] = mk([1, 2, 3]); var x, y; [x, y] = it; r.push(x + "" + y + l.join(""));
var s1 = 1, s2 = 2; [s1, s2] = [s2, s1]; r.push(s1 + "" + s2);
r.join(" ");
