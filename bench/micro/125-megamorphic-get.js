// One load site seeing eight shapes (megamorphic): the field is at different offsets.
// features: loop, object, array, call, literal
// expect: 7200000
function readWeight(o) { return o.w; }
function main() {
  var os = [{ w: 1 }, { a: 0, w: 2 }, { a: 0, b: 0, w: 3 }, { w: 4, a: 0 }, { b: 0, w: 5 },
            { c: 0, a: 0, w: 6 }, { d: 0, w: 7 }, { e: 0, f: 0, g: 0, w: 8 }];
  var s = 0;
  for (var r = 0; r < 200000; r++)
    for (var i = 0; i < os.length; i++) s += readWeight(os[i]);
  return s;
}
main();
