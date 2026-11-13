// A direct call leaves a missing parameter `undefined` when the callee
// only captures it in a closure: liveness counted no closure's capture
// as a read, so compiled code left the previous call's argument there
// (cloison-fuzz BUGS.md #10).
// expect: 0

function f(p) { return (() => p)(); }
function g(a, p) { var h = function () { return p; }; return h; }
var bad = 0;
for (var i = 0; i < 2000; i++) {
  f({ secret: i }); if (f() !== undefined) bad++;
  g(0, i); if (g(0)() !== undefined) bad++;
}
bad;
