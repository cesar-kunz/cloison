// Calling a function stored as a property of a constructor ("static method").
// features: loop, call, object, new, proto, array
// expect: 187500
function Level(n) { this.n = n; }
Level.stronger = function (a, b) { return a.n < b.n; };
function main() {
  var levels = [new Level(0), new Level(3), new Level(6), new Level(2)];
  var n = 0;
  for (var i = 0; i < 500000; i++) {
    if (Level.stronger(levels[i & 3], levels[(i >> 1) & 3])) n++;
  }
  return n;
}
main();
