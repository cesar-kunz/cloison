// A small constructor called in a loop, one field read back.
// toi: direct `new`, `this` allocated in the stencil and typed, unguarded transitions
// features: loop, object, new
// expect: 19999900000
function Point(x, y, z) { this.x = x; this.y = y; this.z = z; }
function main() {
  var s = 0;
  for (var i = 0; i < 200000; i++) s = s + new Point(i, i, i).z;
  return s;
}
main();
