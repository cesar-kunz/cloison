// Calls to methods that do nothing or return a constant, at sites seeing three classes.
// toi: two of the three implementations are empty; inlined, the calls should cost a guard.
// features: loop, call, object, new, proto, array
// expect: 659969
function Stay(v) { this.v = v; this.mark = 0; }
Stay.prototype.isInput = function () { return false; };
Stay.prototype.markInputs = function (m) {};
Stay.prototype.execute = function () {};
function Edit(v) { this.v = v; this.mark = 0; this.edits = 0; }
Edit.prototype.isInput = function () { return true; };
Edit.prototype.markInputs = function (m) {};
Edit.prototype.execute = function () {};
function Copy(v, w) { this.v = v; this.w = w; this.mark = 0; }
Copy.prototype.isInput = function () { return false; };
Copy.prototype.markInputs = function (m) { this.mark = m; };
Copy.prototype.execute = function () { this.w.v = this.v.v; };
function Box(v) { this.v = v; }
function main() {
  var steps = [], boxes = [];
  for (var i = 0; i < 101; i++) boxes.push(new Box(i));
  for (var i = 0; i < 99; i++) {
    var k = i % 3;
    steps.push(k == 0 ? new Stay(boxes[i]) : k == 1 ? new Edit(boxes[i]) : new Copy(boxes[i], boxes[i + 1]));
  }
  var inputs = 0, marks = 0;
  for (var r = 0; r < 10000; r++)
    for (var i = 0; i < 99; i++) {
      var s = steps[i];
      s.markInputs(r);
      s.execute();
      if (s.isInput()) inputs++;
    }
  for (var i = 0; i < 99; i++) marks += steps[i].mark;
  return inputs + marks + boxes[3].v;
}
main();
