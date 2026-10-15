// Adler-32 over a Uint8Array, written through an Int32Array of words (zlib's style).
// features: loop, typed-array
// expect: -489922662
function fillBytes(bytes) {
  for (var i = 0; i < bytes.length; i++) bytes[i] = (i * 7 + (i >> 3)) & 0xff;
}
function adler(bytes) {
  var a = 1, b = 0;
  for (var i = 0; i < bytes.length; i++) {
    a = (a + bytes[i]) % 65521;
    b = (b + a) % 65521;
  }
  return (b << 16) | a;
}
function main() {
  var bytes = new Uint8Array(4096), words = new Int32Array(64), s = 0;
  fillBytes(bytes);
  for (var r = 0; r < 300; r++) {
    words[r & 63] ^= adler(bytes);
    bytes[r] = r & 0xff;
  }
  for (var i = 0; i < 64; i++) s = (s + words[i]) | 0;
  return s;
}
main();
