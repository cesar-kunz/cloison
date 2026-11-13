// An evaluated script's inline caches are dropped once it has run
// (`Code::release`): its calls of a young function logged the caches'
// slots for the next collection, which must not read them after.
// expect: 3600

var sum = 0;
for (var r = 0; r < 400; r++) {
  sum += (0, eval)("var o = { f: function () { return 1; }, g: [1, 2] }; " +
    "var s = 0; for (var i = 0; i < 3; i++) s += o.f() + o.g.length; " +
    "var junk = []; for (var j = 0; j < 3000; j++) junk.push({ j: j, k: [j] }); s");
}
sum
