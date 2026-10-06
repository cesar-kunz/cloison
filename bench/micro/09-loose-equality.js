// `==` / `!=` between objects, null and numbers.
// features: loop, object, null, array
// expect: 2866664
function main() {
  var a = {}, b = {}, objs = [a, b, null, a];
  var n = 0;
  for (var i = 0; i < 400000; i++) {
    var x = objs[i & 3], y = objs[(i >> 2) & 3];
    if (x != null) n++;
    if (x == y) n += 2;
    if (x != b) n += 4;
    if (i % 3 == 1) n += 8;
  }
  return n;
}
main();
