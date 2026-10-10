// GC: most of each nursery survives it. A sliding window keeps the last
// 600,000 records alive (about 19 MiB), so each record outlives a minor
// collection before it dies, already promoted.
// gc: every minor collection traces and promotes nearly all of its 32 MiB
// nursery (about 420,000 cells, 4-5 ms), and the records then die old, so
// the major cycles run back to back: over a third of the time collecting
// for a live set that fits in the nursery.
// features: loop, call, object, new, array
// expect: 978931

function Rec(id, prev) {
  this.id = id;
  this.prev = prev;
  this.a = id & 255;
  this.b = null;
}

function run(n, window) {
  var ring = new Array(window);
  for (var i = 0; i < window; i++) ring[i] = null;
  var sum = 0, prev = null;
  for (var i = 0; i < n; i++) {
    var j = i % window;
    var old = ring[j];
    if (old !== null) sum = (sum + old.a + (old.prev !== null ? old.prev.a : 0)) % 1000003;
    // Each record points at the one before, so the window is one chain.
    var r = new Rec(i, prev);
    if (old !== null) old.prev = null;
    ring[j] = r;
    prev = r;
  }
  return sum;
}

run(12000000, 600000);
