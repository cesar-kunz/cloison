// Dense int switch returning objects held in globals.
// features: loop, switch, object, new, global
// expect: 2100000
function Level(n) { this.n = n; }
var L0 = new Level(0), L1 = new Level(1), L2 = new Level(2),
    L3 = new Level(3), L4 = new Level(4), L5 = new Level(5), L6 = new Level(6);
function next(n) {
  switch (n) {
    case 0: return L6;
    case 1: return L5;
    case 2: return L4;
    case 3: return L3;
    case 4: return L2;
    case 5: return L1;
  }
}
function main() {
  var sum = 0;
  for (var i = 0; i < 600000; i++) sum += next(i % 6).n;
  return sum;
}
main();
