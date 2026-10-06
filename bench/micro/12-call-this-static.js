// A static method calling a sibling through `this` (the constructor).
// features: loop, call, object, new, proto, array
// expect: 2000000
function Level(n) { this.n = n; }
Level.weaker = function (a, b) { return a.n > b.n; };
Level.weakestOf = function (a, b) { return this.weaker(a, b) ? a : b; };
function main() {
  var levels = [new Level(0), new Level(3), new Level(6), new Level(2)];
  var sum = 0;
  for (var i = 0; i < 500000; i++) sum += Level.weakestOf(levels[i & 3], levels[(i >> 1) & 3]).n;
  return sum;
}
main();
