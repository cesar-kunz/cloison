// Two phases taking turns, each building fresh objects for shared functions: ints through two classes, then doubles through three.
// cloison: the shared functions' feedback changes at every phase; their compiled code shouldn't be rebuilt every time.
// features: loop, double, call, object, new, proto, array
// expect: 3479175
function Cell(v) { this.value = v; }
function Keep(a) { this.a = a; }
Keep.prototype.run = function () {};
function Copy(a, b) { this.a = a; this.b = b; }
Copy.prototype.run = function () { this.b.value = this.a.value; };
function Scale(a, b, k, c) { this.k = k; this.c = c; this.a = a; this.b = b; }
Scale.prototype.run = function () { this.b.value = this.a.value * this.k.value + this.c.value; };
function Back(a, b, k, c) { this.a = a; this.b = b; this.k = k; this.c = c; }
Back.prototype.run = function () { this.a.value = (this.b.value - this.c.value) / this.k.value; };
function runAll(steps, n) {
  for (var r = 0; r < n; r++) for (var i = 0; i < steps.length; i++) steps[i].run();
}
function sumAll(cells) {
  var s = 0;
  for (var i = 0; i < cells.length; i++) s += cells[i].value;
  return s;
}
function chainPhase(n) {
  var cells = [], steps = [];
  for (var i = 0; i <= n; i++) cells.push(new Cell(i));
  steps.push(new Keep(cells[0]));
  for (var i = 0; i < n; i++) steps.push(new Copy(cells[i], cells[i + 1]));
  cells[0].value = 7;
  runAll(steps, 20);
  return sumAll(cells);
}
function scalePhase(n) {
  var k = new Cell(0.5), c = new Cell(0.25), cells = [], steps = [];
  for (var i = 0; i < 2 * n; i++) cells.push(new Cell(i));
  for (var i = 0; i < n; i++) {
    var a = cells[2 * i], b = cells[2 * i + 1];
    steps.push(new Keep(a));
    steps.push(i & 1 ? new Back(a, b, k, c) : new Scale(a, b, k, c));
  }
  runAll(steps, 20);
  return sumAll(cells);
}
function main() {
  var total = 0;
  for (var r = 0; r < 150; r++) total += chainPhase(100) + scalePhase(100);
  return total;
}
main();
