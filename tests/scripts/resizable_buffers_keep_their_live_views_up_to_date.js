// A resizable buffer holds its views weakly (`collections::new_weak_set`):
// views made and dropped while it resizes are collected, and those kept
// still follow its length.
// expect: 3820008 16 0  128 16 128

var buf = new ArrayBuffer(64, { maxByteLength: 1024 });
var keep = new Uint8Array(buf), fixed = new Uint8Array(buf, 8, 16), s = 0;
for (var i = 0; i < 30000; i++) {
  var v = new Uint8Array(buf, i & 31, 8);
  v[0] = i & 255;
  s += v[0];
  if ((i & 4095) === 0) buf.resize(64 + (i & 512));
}
buf.resize(16);
var r = [s, keep.length, fixed.length, fixed[0]];
buf.resize(128);
r.concat([keep.length, fixed.length, new DataView(buf).byteLength]).join(" ");
