// Hash mixing with shifts, and, or, xor and unsigned shifts over int32 values.
// features: loop, call
// expect: 341034689
function mixHash(h, k) {
  k = (k ^ (k >>> 16)) & 0xffffffff;
  h ^= k;
  h = (h << 5) | (h >>> 27);
  h = (h + (h << 3)) | 0;
  return h ^ (h >> 7);
}
function main() {
  var h = 17;
  for (var i = 0; i < 2000000; i++) h = mixHash(h, i * 31);
  return h >>> 0;
}
main();
