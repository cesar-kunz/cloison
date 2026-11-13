// An array of more than 1,024 values is marked a slice at a time, the
// rest left on the mark stack (`Marking::drain`): objects reachable only
// from its later slices, stored there young, must survive collections.
// Fails under `CLOISON_GC_STW=1 CLOISON_GC_FULL=1 CLOISON_GC_VERIFY=2048` if the rest
// of the array is dropped after the first slice.
// expect: 626460

function Box(v) { this.v = v; this.next = null; }
function run() {
  var big = new Array(5000), junk = new Array(256);
  for (var i = 0; i < 5000; i++) big[i] = new Box(i);
  for (var i = 0; i < 256; i++) junk[i] = new Box(0);
  var sum = 0;
  for (var round = 0; round < 60; round++) {
    // Garbage that escapes, so collections (and marking) run.
    for (var g = 0; g < 4000; g++) {
      junk[g & 255] = new Box(g);
      sum = (sum + junk[(g * 7) & 255].v) % 1000003;
    }
    // New young objects stored into the array's later slices.
    for (var i = 3000; i < 5000; i += 7) big[i] = new Box(i + round);
    for (var i = 0; i < 5000; i += 13) sum = (sum + big[i].v) % 1000003;
  }
  return sum;
}
run();
