// Deeply nested `v`-mode classes: a SyntaxError, not a native stack
// overflow.
// expect: SyntaxError,true

var r = [];
try { new RegExp('['.repeat(10000) + 'a' + ']'.repeat(10000), 'v'); r.push('no error'); } catch (e) { r.push(e.name); }
r.push(new RegExp('[[[a]]]', 'v').test('a'));
r.join();
