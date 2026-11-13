// A store into an array whose own elements are restricted (frozen,
// sealed or not extensible, an element with attributes, a read-only
// `length`) compiled for its exact shape, a named load before it: the
// shape's elements typed `Any` (`NO_ELEMENTS`) took the cases that check
// nothing (`set_keyed_nc_ia`, `nc_a`, `nc_n`) once the indexed-store
// protector no longer bailed every keyed store (#240, PR #280's Codex
// review). Sloppy and strict, in place and appended, against node.
// expect: 0 1 TypeError

function f(a) { var t = a.tag; a[0] = 9; return a[0]; }
var arr = [1, 2, 3]; arr.tag = 1; Object.freeze(arr);
var bad = 0;
for (var i = 0; i < 20000; i++) if (f(arr) !== 1) bad++;
function g(a) { "use strict"; var t = a.tag; try { a[0] = 9; return "no throw"; } catch (e) { return e.constructor.name; } }
var r; for (var i = 0; i < 20000; i++) r = g(arr);
bad + " " + arr[0] + " " + r
