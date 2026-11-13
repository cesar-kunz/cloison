// A large buffer's bytes are not cleared when allocated (`Heap::alloc_bytes`),
// its blocks zero already: buffers filled, dropped, and their blocks taken
// again by new ones must read zero. Fails under `CLOISON_GC_VERIFY=1024`
// (freed cells poisoned) or `CLOISON_GC_FULL=1` if a reused block isn't
// zeroed.
// expect: 0

var bad = 0;
for (var r = 0; r < 60; r++) {
  var n = (1 + r % 7) * 1024 * 1024 + r * 24;
  var a = new Uint8Array(n);
  for (var i = 0; i < n; i += 4093) if (a[i] !== 0) bad++;
  if (a[n - 1] !== 0) bad++;
  a.fill(0xab);
  var junk = [];
  for (var j = 0; j < 20000; j++) junk.push({x: j});
}
var d = new DataView(new ArrayBuffer(5 << 20));
for (var i = 0; i < (5 << 20); i += 997) if (d.getUint8(i) !== 0) bad++;
bad
