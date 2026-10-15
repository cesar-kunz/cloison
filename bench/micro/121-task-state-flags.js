// Tasks with bit-flag states, a scheduler picking runnable ones (Richards' style).
// features: loop, object, new, proto, null, call
// expect: 8000025
var RUNNABLE = 1, WAITING = 2, HELD = 4;
function Task(id, prio, next) { this.id = id; this.prio = prio; this.state = RUNNABLE; this.count = 0; this.next = next; }
Task.prototype.isHeldOrWaiting = function () { return (this.state & HELD) != 0 || this.state == WAITING; };
Task.prototype.step = function () {
  this.count++;
  if ((this.count & 3) == 0) this.state |= HELD;
  else if ((this.count & 7) == 5) this.state = WAITING;
  return this.next;
};
Task.prototype.release = function () { this.state &= ~HELD; if (this.state == WAITING) this.state = RUNNABLE; };
function schedule(list, rounds) {
  var n = 0;
  for (var r = 0; r < rounds; r++) {
    var t = list;
    while (t !== null) {
      if (t.isHeldOrWaiting()) { t.release(); t = t.next; }
      else { n += t.prio; t = t.step(); }
    }
  }
  return n;
}
function main() {
  var list = null;
  for (var i = 0; i < 10; i++) list = new Task(i, i + 1, list);
  return schedule(list, 200000);
}
main();
