// An object pattern's source is checked by a test and a branch, the
// intrinsic making the error on the path not taken: a function with one
// is compiled and inlined as any other (`intrinsics::is_cold`). A `null`
// or `undefined` source still throws there, from compiled code at any
// depth, with the interpreter's message; so does an empty pattern's. A
// strict `delete` that fails, as cold, throws from compiled code too.
// expect: 3322800|Cannot destructure 'null' as it is null.|Cannot destructure 'undefined' as it is undefined.|Cannot destructure 'null' as it is null.|Cannot delete a non-configurable property|7
"use strict";
function area(r) { const { a, b, c = 2 } = r; return a * b + c; }
function empty(r) { const {} = r; return 1; }
function outer(r) { return area(r) + empty(r); }
function drop(o, k, really) { if (really) delete o[k]; return o[k]; }
var rs = [];
for (var i = 0; i < 20; i++) rs.push({ a: i, b: i + 1, c: i % 3 ? undefined : i });
var out = [], t = 0;
for (var k = 0; k < 1200; k++) {
  for (var i = 0; i < 20; i++) t += outer(rs[i]);
  if (k == 600 || k == 1100) {
    try { t += outer(k == 600 ? null : undefined); } catch (e) { out.push(e.message); }
  }
}
out.unshift(t);
try { empty(null); } catch (e) { out.push(e.message); }
var frozen = Object.freeze({ x: 7 });
for (var k = 0; k < 3000; k++) drop(frozen, "x", false);
try { drop(frozen, "x", true); } catch (e) { out.push(e.message); }
out.push(drop(frozen, "x", false));
out.join("|");
