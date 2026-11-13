// In an async function or a generator, a block-scoped variable read
// before its declaration through a closure is a ReferenceError (its box
// or scope object slot starts as the hole, not `undefined`).
// expect: ReferenceError,none,ReferenceError,none

var out = [];
function t(f) { try { f(); out.push("none"); } catch (e) { out.push(e.name); } }
async function a() { function f() { return x; } t(f); const x = 1; t(f); }
function* g() { { function f() { return y; } t(f); let y = 2; t(f); yield 0; } }
a(); [...g()];
out.join();
