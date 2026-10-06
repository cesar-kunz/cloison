// do-while stepping through levels until a sentinel, with an inner counted loop.
// features: loop
// expect: 137143
function main() {
  var hits = 0;
  for (var r = 0; r < 20000; r++) {
    var level = 0;
    do {
      for (var i = 0; i < 8; i++) if ((i + r) % 7 == level) hits++;
      level++;
    } while (level != 6);
  }
  return hits;
}
main();
