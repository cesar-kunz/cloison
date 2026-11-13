// `for … of` closes its iterator where the loop ends early (a `break`, a
// `continue` past it, a throw in the body), not where the iterator
// itself fails (`next()`, a result's `done` or `value`, a non-object).
// expect: n n n n nR nR nnn

var r = [];
function mk(next) { var log = []; return [log, { [Symbol.iterator]() { return { next() { log.push("n"); return next(log); }, return() { log.push("R"); return {}; } }; } }]; }
function run(next, loop) { var [log, it] = mk(next); try { loop(it); } catch (e) {} return log.join(""); }
r.push(run(() => { throw 1; }, it => { for (var v of it); }));
r.push(run(() => ({ get done() { throw 1; } }), it => { for (var v of it); }));
r.push(run(() => ({ done: false, get value() { throw 1; } }), it => { for (var v of it); }));
r.push(run(() => 5, it => { for (var v of it); }));
r.push(run(() => ({ done: false, value: 1 }), it => { for (var v of it) throw 1; }));
r.push(run(() => ({ done: false, value: 1 }), it => { for (var v of it) break; }));
r.push(run(log => log.length > 2 ? { done: true } : { done: false, value: 1 }, it => { for (var v of it); }));
r.join(" ");
