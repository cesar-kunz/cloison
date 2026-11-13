// `push` past the largest array index stores a named property and then
// throws a RangeError for the length, with no protector set: the far
// path stored an index and kept the length (hidden while any frozen
// array protected indexed stores, #240).
// expect: RangeError,4294967295,1,RangeError,4294967295,1,2

function p(a, v) { return a.push(v); }
for (var i = 0; i < 20000; i++) p([1], i);
var out = [];
var t = []; t.length = 4294967295;
try { p(t, 1); } catch (e) { out.push(e.constructor.name, t.length, t[4294967295]); }
var u = []; u.length = 4294967294;
try { u.push(1, 2); } catch (e) { out.push(e.constructor.name, u.length, u[4294967294], u[4294967295]); }
out.join();
